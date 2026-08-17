//! `gsymtool` command lines, from argument handling to end-to-end conversions.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gsym::{AddressRange, Endian, Function, Gsym, GsymBuilder, GsymVersion};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gsymtool"))
        .args(arguments)
        .output()
        .expect("run gsymtool")
}

fn run_ok(arguments: &[&str]) -> Output {
    crate::tools::run(Command::new(env!("CARGO_BIN_EXE_gsymtool")).args(arguments))
}

fn run_ok_in(directory: &Path, arguments: &[&str]) -> Output {
    crate::tools::run(
        Command::new(env!("CARGO_BIN_EXE_gsymtool"))
            .current_dir(directory)
            .args(arguments),
    )
}

fn warning_lines(output: &Output) -> Vec<&str> {
    std::str::from_utf8(&output.stderr)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("warning:"))
        .collect()
}

/// Compile a tiny relocatable ELF with DWARF, named so the batch can find it.
fn compile_object(sources: &Path, name: &str, output: &Path) {
    let source = sources.join(format!("{name}.c"));
    std::fs::write(
        &source,
        format!("__attribute__((noinline)) int {name}(int value) {{ return value + 1; }}\n"),
    )
    .unwrap();
    crate::tools::run(Command::new(crate::tools::required_tool("clang")).args([
        "-g",
        "-O1",
        "-c",
        "-o",
        output.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
}

fn build_warning_fixture(root: &Path) -> PathBuf {
    let yaml = root.join("warning-fixture.yaml");
    let object = root.join("warning-fixture");
    let text = root.join("text.bin");
    let image = root.join("warning-fixture-with-text");
    std::fs::write(
        &yaml,
        include_str!("../../../tests/fixtures/linker_tombstones.yaml"),
    )
    .unwrap();
    std::fs::write(&text, vec![0_u8; 0x100]).unwrap();
    crate::tools::run(Command::new(crate::tools::required_tool("yaml2obj")).args([
        yaml.to_str().unwrap(),
        "-o",
        object.to_str().unwrap(),
    ]));
    crate::tools::run(
        Command::new(crate::tools::required_tool("x86_64-linux-gnu-objcopy")).args([
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
    image
}

/// Build `tree/alpha.bin`, `tree/sub/beta.bin` and one file that is not an ELF.
fn build_input_tree(root: &Path) -> PathBuf {
    let sources = root.join("src");
    let tree = root.join("tree");
    std::fs::create_dir_all(&sources).unwrap();
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    compile_object(&sources, "alpha", &tree.join("alpha.bin"));
    compile_object(&sources, "beta", &tree.join("sub").join("beta.bin"));
    std::fs::write(tree.join("notes.txt"), b"not an ELF image").unwrap();
    tree
}

/// Parse a converted file and confirm it holds the function it was built from.
fn assert_converted(path: &Path, function: &[u8]) {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("{} was not written: {error}", path.display()));
    let gsym = Gsym::parse(&bytes).unwrap();
    gsym.verify().unwrap();
    assert!(
        gsym.functions()
            .filter_map(Result::ok)
            .any(|candidate| candidate.name() == function),
        "{} is missing {}",
        path.display(),
        String::from_utf8_lossy(function)
    );
}

#[test]
fn cmdline_converts_a_directory_tree_in_parallel() {
    let directory = tempfile::tempdir().unwrap();
    let tree = build_input_tree(directory.path());
    let output = directory.path().join("out");

    let batch = run_ok(&[
        "convert",
        tree.to_str().unwrap(),
        "--output-dir",
        output.to_str().unwrap(),
        "--recursive",
        "--jobs",
        "4",
    ]);

    let stderr = String::from_utf8_lossy(&batch.stderr);
    assert!(
        stderr.contains("scanning 1 input for ELF files"),
        "{stderr}"
    );
    assert!(stderr.contains("converting 2 ELF files"), "{stderr}");
    assert!(stderr.contains("converted    2 of 2 files"), "{stderr}");
    assert!(stderr.contains("1 non-ELF path"), "{stderr}");
    assert!(!stderr.contains("ok:"), "{stderr}");
    assert_converted(&output.join("alpha.bin.gsym"), b"alpha");
    assert_converted(&output.join("sub").join("beta.bin.gsym"), b"beta");
    assert!(!output.join("notes.txt.gsym").exists());
}

#[test]
fn cmdline_batch_needs_recursion_for_subdirectories() {
    let directory = tempfile::tempdir().unwrap();
    let tree = build_input_tree(directory.path());
    let output = directory.path().join("shallow");

    run_ok(&[
        "convert",
        tree.to_str().unwrap(),
        "--output-dir",
        output.to_str().unwrap(),
    ]);

    assert_converted(&output.join("alpha.bin.gsym"), b"alpha");
    assert!(!output.join("sub").join("beta.bin.gsym").exists());
}

#[test]
fn cmdline_batch_converts_what_it_can_and_reports_the_rest() {
    let directory = tempfile::tempdir().unwrap();
    let tree = build_input_tree(directory.path());
    let output = directory.path().join("out");
    std::fs::write(tree.join("broken.bin"), b"\x7fELF and nothing else").unwrap();
    std::fs::write(tree.join("empty.bin"), b"\x7fELF").unwrap();

    let batch = run(&[
        "convert",
        tree.to_str().unwrap(),
        "--output-dir",
        output.to_str().unwrap(),
    ]);

    assert!(!batch.status.success());
    let stderr = String::from_utf8_lossy(&batch.stderr);
    assert!(stderr.contains("broken.bin"), "{stderr}");
    assert!(stderr.contains("conversions failed"), "{stderr}");
    assert_converted(&output.join("alpha.bin.gsym"), b"alpha");
}

#[test]
fn cmdline_batch_rejects_inputs_that_claim_one_output() {
    let directory = tempfile::tempdir().unwrap();
    let sources = directory.path().join("src");
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::create_dir_all(&sources).unwrap();
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    compile_object(&sources, "alpha", &first.join("app.bin"));
    compile_object(&sources, "beta", &second.join("app.bin"));
    let output = directory.path().join("out");

    let batch = run(&[
        "convert",
        first.join("app.bin").to_str().unwrap(),
        second.join("app.bin").to_str().unwrap(),
        "--output-dir",
        output.to_str().unwrap(),
    ]);

    assert!(!batch.status.success());
    let stderr = String::from_utf8_lossy(&batch.stderr);
    assert!(stderr.contains("would both be written to"), "{stderr}");
    assert!(!output.join("app.bin.gsym").exists());
}

#[test]
fn cmdline_rejects_missing_and_invalid_arguments() {
    let no_command = run(&[]);
    assert!(!no_command.status.success());
    assert!(String::from_utf8_lossy(&no_command.stderr).contains("Usage:"));

    let no_addresses = run(&["lookup", "input.gsym"]);
    assert!(!no_addresses.status.success());
    assert!(String::from_utf8_lossy(&no_addresses.stderr).contains("<ADDRESS>"));

    let bad_address = run(&["lookup", "input.gsym", "0xnot-an-address"]);
    assert!(!bad_address.status.success());
    assert!(String::from_utf8_lossy(&bad_address.stderr).contains("invalid address"));

    let orphan_limit = run(&["dump", "input.gsym", "--limit", "1"]);
    assert!(!orphan_limit.status.success());
    assert!(String::from_utf8_lossy(&orphan_limit.stderr).contains("--functions"));

    let bad_version = run(&[
        "convert",
        "input.elf",
        "--output",
        "output.gsym",
        "--version",
        "v3",
    ]);
    assert!(!bad_version.status.success());
    assert!(String::from_utf8_lossy(&bad_version.stderr).contains("invalid value 'v3'"));
}

#[test]
fn terminal_color_overrides_and_completion_generation_work_when_piped() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("tiny.gsym");
    let mut builder = GsymBuilder::new();
    builder
        .add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"tiny"))
        .unwrap();
    std::fs::write(&input, builder.to_bytes().unwrap()).unwrap();

    let automatic = run_ok(&["dump", input.to_str().unwrap()]);
    assert!(!automatic.stdout.contains(&0x1b));

    let colored = run_ok(&["--color", "always", "dump", input.to_str().unwrap()]);
    assert!(colored.stdout.contains(&0x1b));

    let colored_help = run_ok(&["--color", "always", "--help"]);
    assert!(colored_help.stdout.contains(&0x1b));

    let completions = run_ok(&["completions", "bash"]);
    assert!(String::from_utf8_lossy(&completions.stdout).contains("_gsymtool"));
}

#[test]
fn conversion_warning_detail_obeys_output_mode() {
    let directory = tempfile::tempdir().unwrap();
    let image = build_warning_fixture(directory.path());
    let image = image.to_str().unwrap();
    let convert = |mode: &[&str], name| {
        let output = directory.path().join(name);
        let mut arguments = mode.to_vec();
        arguments.extend_from_slice(&[
            "convert",
            image,
            "-o",
            output.to_str().unwrap(),
            "--no-debuginfod",
        ]);
        run_ok(&arguments)
    };

    let normal = convert(&[], "normal.gsym");
    assert_eq!(
        warning_lines(&normal),
        [
            "warning: no valid DWARF inline records were found",
            "warning: skipping 1 non-live or invalid DWARF range; examples: 0x3000..0x3020; pass --verbose to show each unexpected rejected range",
        ]
    );

    let quiet = convert(&["--quiet"], "quiet.gsym");
    assert_eq!(warning_lines(&quiet), Vec::<&str>::new());

    let verbose = convert(&["--verbose"], "verbose.gsym");
    assert_eq!(
        warning_lines(&verbose),
        [
            "warning: skipping non-live or invalid DWARF range 0x3000..0x3020",
            "warning: no valid DWARF inline records were found",
        ]
    );
}

#[test]
fn convert_failure_preserves_an_existing_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("invalid.elf");
    let output = directory.path().join("symbols.gsym");
    std::fs::write(&input, b"not an ELF image").unwrap();
    std::fs::write(&output, b"existing output must survive").unwrap();

    let result = run(&[
        "convert",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"existing output must survive"
    );

    let mut entries = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries, ["invalid.elf", "symbols.gsym"]);
}

#[test]
fn cmdline_convert_lookup_dump_and_verify_end_to_end() {
    let directory = tempfile::tempdir().unwrap();
    let image = std::env::current_exe().unwrap();
    let output = directory.path().join("image.gsym");
    std::fs::write(&output, b"replace me atomically").unwrap();

    let conversion = run_ok(&[
        "convert",
        image.to_str().unwrap(),
        "--symbols",
        image.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--version",
        "v2",
    ]);
    assert!(String::from_utf8_lossy(&conversion.stderr).contains("wrote"));

    let output_bytes = std::fs::read(&output).unwrap();
    let output_gsym = Gsym::parse(&output_bytes).unwrap();
    output_gsym.verify().unwrap();
    let function = output_gsym
        .functions()
        .filter_map(Result::ok)
        .find(|function| !function.range().is_empty())
        .expect("converted ELF contains a nonempty function");
    let address = format!("{:#x}", function.start());
    let expected_name = String::from_utf8_lossy(function.name());

    let lookup = run_ok(&["lookup", output.to_str().unwrap(), &address]);
    let lookup_stdout = String::from_utf8_lossy(&lookup.stdout);
    assert!(lookup_stdout.contains(&*expected_name));

    let dump = run_ok(&[
        "dump",
        output.to_str().unwrap(),
        "--functions",
        "--limit",
        "1",
    ]);
    let dump_stdout = String::from_utf8_lossy(&dump.stdout);
    assert!(dump_stdout.contains("GSYM v2"));
    assert!(dump_stdout.contains("Functions"));

    let verify = run_ok(&["verify", output.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&verify.stdout).contains("valid"));
}

#[test]
fn no_discovery_keeps_split_dwarf_out_of_cli_conversion() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("split.c"),
        "__attribute__((noinline)) int split_target(int value) { return value + 1; }\nint main(void) { return split_target(1); }\n",
    )
    .unwrap();
    crate::tools::run(
        Command::new(crate::tools::required_tool("gcc"))
            .current_dir(directory.path())
            .args(["-g", "-O1", "-gsplit-dwarf", "-o", "split", "split.c"]),
    );
    assert!(std::fs::read_dir(directory.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "dwo")
    }));

    let discovered = run_ok_in(
        directory.path(),
        &["convert", "split", "-o", "discovered.gsym"],
    );
    let discovered_stderr = String::from_utf8_lossy(&discovered.stderr);
    assert!(
        discovered_stderr.contains("debug info"),
        "{discovered_stderr}"
    );
    assert!(
        !discovered_stderr.contains("| 0 DWARF") && !discovered_stderr.contains("| 0 line rows"),
        "{discovered_stderr}"
    );

    let isolated = run_ok_in(
        directory.path(),
        &["convert", "split", "-o", "isolated.gsym", "--no-discovery"],
    );
    let isolated_stderr = String::from_utf8_lossy(&isolated.stderr);
    assert!(isolated_stderr.contains("| 0 DWARF"), "{isolated_stderr}");
    assert!(
        isolated_stderr.contains("| 0 line rows | 0 inline calls"),
        "{isolated_stderr}"
    );
    assert!(
        isolated_stderr.contains("has no usable .dwo or .dwp data"),
        "{isolated_stderr}"
    );

    let bytes = std::fs::read(directory.path().join("isolated.gsym")).unwrap();
    Gsym::parse(&bytes).unwrap().verify().unwrap();
}

#[test]
fn cmdline_transcodes_and_segments_existing_gsym_files() {
    let directory = tempfile::tempdir().unwrap();
    let image = std::env::current_exe().unwrap();
    let source = directory.path().join("source.gsym");
    run_ok(&[
        "convert",
        image.to_str().unwrap(),
        "--symbols",
        image.to_str().unwrap(),
        "--output",
        source.to_str().unwrap(),
    ]);

    let transcoded = directory.path().join("transcoded.gsym");
    run_ok(&[
        "transcode",
        source.to_str().unwrap(),
        "--output",
        transcoded.to_str().unwrap(),
        "--version",
        "v2",
        "--endian",
        "big",
    ]);
    let transcoded_bytes = std::fs::read(&transcoded).unwrap();
    let transcoded_gsym = Gsym::parse(&transcoded_bytes).unwrap();
    assert_eq!(transcoded_gsym.header().version, GsymVersion::V2);
    assert_eq!(transcoded_gsym.header().endian, Endian::Big);
    transcoded_gsym.verify().unwrap();

    let prefix = directory.path().join("shard.gsym");
    run_ok(&[
        "segment",
        transcoded.to_str().unwrap(),
        "--output",
        prefix.to_str().unwrap(),
        "--size",
        "32MiB",
    ]);
    let shards = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("shard.gsym-0x")
        })
        .collect::<Vec<_>>();
    assert!(!shards.is_empty());
    for shard in shards {
        let bytes = std::fs::read(shard).unwrap();
        Gsym::parse(&bytes).unwrap().verify().unwrap();
    }
}

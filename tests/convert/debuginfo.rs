//! Locating debug info that lives outside the image: debuglink, build-id
//! directories, debuginfod and the compressed `.gnu_debugdata` section.

use std::fmt::Write as _;
use std::process::Command;

use gsym::convert::{ConversionOptions, ConversionWarning, DiscoveryEvent, ElfConverter};
use object::Object;

use crate::elf::retarget_machine;
use crate::tools::run;

fn compile_debug_pair(directory: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let source = directory.join("discover.c");
    let image = directory.join("discover");
    let debug = directory.join("discover.debug");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int discovered(int x) { return x + 1; }\nint main(void) { return discovered(1); }\n",
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
    run(Command::new("objcopy").args([
        "--only-keep-debug",
        image.to_str().unwrap(),
        debug.to_str().unwrap(),
    ]));
    (image, debug)
}

fn strip_debug(directory: &std::path::Path, image: &std::path::Path) -> std::path::PathBuf {
    let stripped = directory.join("discover.stripped");
    run(Command::new("objcopy").args([
        "--strip-debug",
        image.to_str().unwrap(),
        stripped.to_str().unwrap(),
    ]));
    stripped
}

fn xz_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut input = std::io::Cursor::new(bytes);
    let mut compressed = Vec::new();
    lzma_rs::xz_compress(&mut input, &mut compressed).unwrap();
    compressed
}

fn embed_gnu_debugdata(
    directory: &std::path::Path,
    image: &std::path::Path,
    payload: &[u8],
    output_name: &str,
) -> std::path::PathBuf {
    let payload_path = directory.join(format!("{output_name}.xz"));
    let output = directory.join(output_name);
    std::fs::write(&payload_path, payload).unwrap();
    run(Command::new("objcopy").args([
        "--strip-debug",
        "--add-section",
        &format!(".gnu_debugdata={}", payload_path.display()),
        image.to_str().unwrap(),
        output.to_str().unwrap(),
    ]));
    output
}

#[test]
fn imports_xz_compressed_gnu_debugdata() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let debug_bytes = std::fs::read(debug).unwrap();
    let embedded = embed_gnu_debugdata(
        directory.path(),
        &image,
        &xz_bytes(&debug_bytes),
        "discover.mini-debug",
    );

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&embedded)
        .unwrap();
    assert!(report.stats.dwarf_functions > 0, "{:?}", report.warnings);
    assert!(
        report
            .builder
            .functions()
            .iter()
            .any(|function| function.name == b"discovered" && !function.lines.is_empty())
    );
}

#[test]
fn bounds_gnu_debugdata_decompression_and_keeps_symbol_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let debug_bytes = std::fs::read(debug).unwrap();
    let embedded = embed_gnu_debugdata(
        directory.path(),
        &image,
        &xz_bytes(&debug_bytes),
        "discover.mini-debug",
    );
    let options = ConversionOptions {
        gnu_debugdata_max_decompressed_size: 64,
        ..ConversionOptions::default()
    };

    let report = ElfConverter::new(options).convert_path(&embedded).unwrap();
    assert!(report.stats.symbol_functions > 0);
    assert!(report.warnings.iter().any(|warning| matches!(
        warning,
        ConversionWarning::EmbeddedDebugData { reason } if reason.contains("64-byte limit")
    )));
}

#[test]
fn malformed_mini_debug_payloads_are_ignored_without_losing_symbols() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let debug_bytes = std::fs::read(debug).unwrap();

    let cases = [
        ("invalid-xz", b"not an xz stream".to_vec()),
        ("truncated-elf", xz_bytes(&debug_bytes[..64])),
    ];
    for (name, payload) in cases {
        let embedded = embed_gnu_debugdata(directory.path(), &image, &payload, name);
        let report = ElfConverter::new(ConversionOptions::default())
            .convert_path(&embedded)
            .unwrap();
        assert!(report.stats.symbol_functions > 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| matches!(warning, ConversionWarning::EmbeddedDebugData { .. })),
            "missing warning for {name}: {:?}",
            report.warnings
        );
    }
}

#[test]
fn mini_debug_with_the_wrong_architecture_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let mut debug_bytes = std::fs::read(debug).unwrap();
    retarget_machine(&mut debug_bytes);
    let embedded = embed_gnu_debugdata(
        directory.path(),
        &image,
        &xz_bytes(&debug_bytes),
        "wrong-architecture-mini-debug",
    );

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&embedded)
        .unwrap();
    assert!(report.stats.symbol_functions > 0);
    assert!(report.warnings.iter().any(|warning| matches!(
        warning,
        ConversionWarning::EmbeddedDebugData { reason } if reason.contains("architecture")
    )));
}

#[test]
fn mini_debug_symbols_are_imported_with_dwarf_disabled() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let payload = directory.path().join("symbols-only-mini-debug.xz");
    let stripped = directory.path().join("discover.fully-stripped");
    let embedded = directory.path().join("discover.symbols-only");
    std::fs::write(&payload, xz_bytes(&std::fs::read(&debug).unwrap())).unwrap();
    run(Command::new("objcopy").args([
        "--strip-all",
        image.to_str().unwrap(),
        stripped.to_str().unwrap(),
    ]));
    run(Command::new("objcopy").args([
        "--add-section",
        &format!(".gnu_debugdata={}", payload.display()),
        stripped.to_str().unwrap(),
        embedded.to_str().unwrap(),
    ]));
    let options = ConversionOptions {
        dwarf: None,
        ..ConversionOptions::default()
    };

    let report = ElfConverter::new(options).convert_path(&embedded).unwrap();

    assert_eq!(report.stats.dwarf_functions, 0);
    assert!(report.stats.symbol_functions > 0, "{:?}", report.warnings);
    assert!(
        report
            .builder
            .functions()
            .iter()
            .any(|function| function.name == b"discovered"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn mini_debug_does_not_require_build_ids() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("no-build-id.c");
    let image = directory.path().join("no-build-id");
    let debug = directory.path().join("no-build-id.debug");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int no_build_id(int x) { return x + 1; }\nint main(void) { return no_build_id(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-g",
        "-O1",
        "-Wl,--build-id=none",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    run(Command::new("objcopy").args([
        "--only-keep-debug",
        image.to_str().unwrap(),
        debug.to_str().unwrap(),
    ]));
    let debug_bytes = std::fs::read(debug).unwrap();
    let embedded = embed_gnu_debugdata(
        directory.path(),
        &image,
        &xz_bytes(&debug_bytes),
        "no-build-id-mini-debug",
    );

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&embedded)
        .unwrap();
    assert!(report.builder.options().writer.build_id.is_empty());
    assert!(
        report
            .builder
            .functions()
            .iter()
            .any(|function| { function.name == b"no_build_id" && !function.lines.is_empty() })
    );
}

#[cfg(feature = "debuginfod")]
fn serve_debuginfo_once(body: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.ends_with(b"\r\n\r\n") {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (format!("http://{address}"), handle)
}

#[test]
fn discovers_and_validates_gnu_debuglink_companions() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let stripped = directory.path().join("discover.stripped");
    run(Command::new("objcopy").current_dir(directory.path()).args([
        "--strip-debug",
        "--add-gnu-debuglink=discover.debug",
        image.to_str().unwrap(),
        stripped.to_str().unwrap(),
    ]));

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&stripped)
        .unwrap();
    assert_eq!(report.discovered_debug.as_deref(), Some(debug.as_path()));
    assert!(report.stats.dwarf_functions > 0);
}

#[test]
fn discovers_build_id_debug_files_under_configured_roots() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let stripped = strip_debug(directory.path(), &image);
    let image_bytes = std::fs::read(&stripped).unwrap();
    let parsed = object::File::parse(image_bytes.as_slice()).unwrap();
    let build_id = parsed.build_id().unwrap().unwrap();
    let hex = build_id.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    });
    let destination = directory
        .path()
        .join("debug-root/.build-id")
        .join(
            hex.get(..2)
                .expect("a build ID renders to at least one byte"),
        )
        .join(format!(
            "{}.debug",
            hex.get(2..)
                .expect("a build ID renders to at least one byte")
        ));
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::copy(&debug, &destination).unwrap();

    let options = ConversionOptions {
        debug_directories: vec![directory.path().join("debug-root")],
        ..ConversionOptions::default()
    };
    let report = ElfConverter::new(options).convert_path(&stripped).unwrap();
    assert_eq!(
        report.discovered_debug.as_deref(),
        Some(destination.as_path())
    );
    assert!(report.stats.dwarf_functions > 0);
}

#[cfg(feature = "debuginfod")]
#[test]
fn downloads_validated_debuginfo_and_reuses_the_cache_offline() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let stripped = strip_debug(directory.path(), &image);
    let debug_bytes = std::fs::read(debug).unwrap();
    let (server, handle) = serve_debuginfo_once(debug_bytes);
    let expected_server = server.clone();
    let options = ConversionOptions {
        debug_directories: Vec::new(),
        debuginfod_urls: vec![server],
        debuginfod_cache: directory.path().join("cache"),
        ..ConversionOptions::default()
    };
    let converter = ElfConverter::new(options);

    let mut requests = Vec::new();
    let downloaded = converter
        .convert_path_with_observer(&stripped, |event| {
            if let DiscoveryEvent::DebuginfodRequest {
                artifact,
                build_id,
                endpoint,
                related_path,
            } = event
            {
                requests.push((
                    artifact,
                    build_id.to_owned(),
                    endpoint.to_owned(),
                    related_path.to_path_buf(),
                ));
            }
        })
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "debug file");
    assert!(!requests[0].1.is_empty());
    assert_eq!(requests[0].2, expected_server);
    assert_eq!(requests[0].3, stripped);
    assert!(downloaded.stats.dwarf_functions > 0);
    let cache_path = downloaded
        .discovered_debug
        .as_deref()
        .expect("downloaded debug file must be cached");
    assert!(cache_path.starts_with(directory.path().join("cache")));
    assert!(cache_path.is_file());
    let request = handle.join().unwrap();
    assert!(request.starts_with("GET /buildid/"));
    assert!(request.contains("/debuginfo HTTP/1.1"));

    let cached = converter.convert_path(&stripped).unwrap();
    assert_eq!(cached.discovered_debug.as_deref(), Some(cache_path));
    assert!(cached.stats.dwarf_functions > 0);
}

#[cfg(feature = "debuginfod")]
#[test]
fn coalesces_repeated_debuginfod_requests_for_one_build_id() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let stripped = strip_debug(directory.path(), &image);
    let (server, handle) = serve_debuginfo_once(std::fs::read(debug).unwrap());
    let converter = ElfConverter::new(ConversionOptions {
        debug_directories: Vec::new(),
        debuginfod_urls: vec![server],
        debuginfod_cache: directory.path().join("cache"),
        ..ConversionOptions::default()
    });
    let request_count = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        let conversions = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    converter
                        .convert_path_with_observer(&stripped, |_| {
                            request_count.fetch_add(1, Ordering::Relaxed);
                        })
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for conversion in conversions {
            assert!(conversion.join().unwrap().stats.dwarf_functions > 0);
        }
    });

    handle.join().unwrap();
    assert_eq!(request_count.load(Ordering::Relaxed), 1);
}

#[cfg(feature = "debuginfod")]
#[test]
fn rejects_oversized_debuginfod_responses_without_losing_symbols() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let stripped = strip_debug(directory.path(), &image);
    let debug_bytes = std::fs::read(debug).unwrap();
    let (server, handle) = serve_debuginfo_once(debug_bytes);
    let options = ConversionOptions {
        debug_directories: Vec::new(),
        debuginfod_urls: vec![server],
        debuginfod_cache: directory.path().join("cache"),
        debuginfod_max_download_size: 64,
        ..ConversionOptions::default()
    };

    let report = ElfConverter::new(options).convert_path(&stripped).unwrap();
    handle.join().unwrap();
    assert!(report.discovered_debug.is_none());
    assert!(report.stats.symbol_functions > 0);
    assert!(report.warnings.iter().any(|warning| matches!(
        warning,
        ConversionWarning::Debuginfod { reason, .. }
            if reason.contains("response") && reason.contains("rejected")
    )));
}

#[cfg(feature = "debuginfod")]
#[test]
fn rejects_debuginfod_responses_with_the_wrong_build_id() {
    let directory = tempfile::tempdir().unwrap();
    let (image, debug) = compile_debug_pair(directory.path());
    let stripped = strip_debug(directory.path(), &image);
    let mut debug_bytes = std::fs::read(debug).unwrap();
    let parsed = object::File::parse(debug_bytes.as_slice()).unwrap();
    let build_id = parsed.build_id().unwrap().unwrap().to_vec();
    let position = debug_bytes
        .windows(build_id.len())
        .position(|window| window == build_id)
        .unwrap();
    debug_bytes[position] ^= 0x80;
    let (server, handle) = serve_debuginfo_once(debug_bytes);
    let options = ConversionOptions {
        debug_directories: Vec::new(),
        debuginfod_urls: vec![server],
        debuginfod_cache: directory.path().join("cache"),
        ..ConversionOptions::default()
    };

    let report = ElfConverter::new(options).convert_path(&stripped).unwrap();
    handle.join().unwrap();
    assert!(report.discovered_debug.is_none());
    assert!(report.stats.symbol_functions > 0);
    assert!(report.warnings.iter().any(|warning| matches!(
        warning,
        ConversionWarning::Debuginfod { reason, .. }
            if reason.contains("does not match") && reason.contains("build ID or architecture")
    )));
}

/// Assembles a `.gnu_debugaltlink` pair and converts the image.
fn convert_debugaltlink_pair(
    directory: &std::path::Path,
    linked_name: &str,
) -> gsym::convert::ConversionReport {
    let yaml2obj = crate::tools::required_tool("yaml2obj");
    let build = |source: &str, name: &str| {
        let yaml = directory.join(format!("{name}.yaml"));
        let object = directory.join(name);
        std::fs::write(&yaml, source).unwrap();
        run(Command::new(&yaml2obj).args([yaml.to_str().unwrap(), "-o", object.to_str().unwrap()]));
        object
    };
    let image = build(
        include_str!("../fixtures/debugaltlink_image.yaml"),
        "altlink.image",
    );
    let supplementary = build(
        include_str!("../fixtures/debugaltlink_supplementary.yaml"),
        "supplementary.debug",
    );

    let bytes = std::fs::read(&supplementary).unwrap();
    let parsed = object::File::parse(bytes.as_slice()).unwrap();
    let mut section = linked_name.as_bytes().to_vec();
    section.push(0);
    section.extend_from_slice(parsed.build_id().unwrap().unwrap());
    let payload = directory.join("debugaltlink.bin");
    std::fs::write(&payload, section).unwrap();

    let linked = directory.join("altlink.linked");
    run(
        Command::new(crate::tools::required_tool("x86_64-linux-gnu-objcopy")).args([
            "--add-section",
            &format!(".gnu_debugaltlink={}", payload.display()),
            image.to_str().unwrap(),
            linked.to_str().unwrap(),
        ]),
    );
    ElfConverter::new(ConversionOptions::default())
        .convert_path(&linked)
        .unwrap()
}

#[test]
fn resolves_names_through_gnu_debugaltlink_supplementary_files() {
    let directory = tempfile::tempdir().unwrap();
    let report = convert_debugaltlink_pair(directory.path(), "supplementary.debug");

    assert_eq!(
        report.discovered_supplementary.as_deref(),
        Some(directory.path().join("supplementary.debug").as_path())
    );
    assert_eq!(report.stats.dwarf_functions, 1, "{:?}", report.warnings);
    let function = report
        .builder
        .functions()
        .iter()
        .find(|function| function.name == b"supplementary_origin")
        .expect("the name must be read from the supplementary object");
    assert_eq!(function.range, gsym::AddressRange::new(0x1000, 0x1100));
}

/// A link nothing resolves must leave the reference unresolved, not invented.
#[test]
fn unresolved_gnu_debugaltlink_links_convert_without_supplementary_names() {
    let directory = tempfile::tempdir().unwrap();
    let report = convert_debugaltlink_pair(directory.path(), "absent.debug");

    assert!(report.discovered_supplementary.is_none());
    assert!(
        report
            .builder
            .functions()
            .iter()
            .all(|function| function.name != b"supplementary_origin"),
        "a name was resolved without the supplementary object"
    );
}

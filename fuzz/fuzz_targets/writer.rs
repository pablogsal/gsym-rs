#![no_main]

use std::collections::HashSet;

use arbitrary::Arbitrary;
use gsym::{
    AddressRange, CallSite, CallSiteFlags, Endian, FileEntry, FileIndex, Function,
    FunctionSetPolicy, Gsym, GsymBuilder, GsymVersion, InlineNode, LineEntry, TranscodeOptions,
};
use libfuzzer_sys::fuzz_target;

const MAX_FUNCTIONS: usize = 12;
const MAX_LINES: usize = 16;
const MAX_INLINE_DEPTH: usize = 6;
const MAX_CALL_SITES: usize = 6;
const MAX_PATTERNS: usize = 4;
const MAX_STRING: usize = 48;

/// Number of address slots generated functions are laid out in.
const SLOTS: u64 = 6;

/// Size of one address slot, which also bounds a generated function.
const SLOT_SIZE: u64 = 0x100;

const BASE_ADDRESS: u64 = 0x1000;

#[derive(Arbitrary, Debug)]
struct Case {
    control: u8,
    executable_tail: u8,
    files: Vec<FileSeed>,
    functions: Vec<FunctionSeed>,
}

#[derive(Arbitrary, Debug)]
struct FileSeed {
    directory: Vec<u8>,
    basename: Vec<u8>,
}

#[derive(Arbitrary, Debug)]
struct FunctionSeed {
    slot: u8,
    width: u8,
    style: u8,
    name: Vec<u8>,
    lines: Vec<LineSeed>,
    inline: Vec<InlineSeed>,
    call_sites: Vec<CallSiteSeed>,
}

#[derive(Arbitrary, Debug)]
struct LineSeed {
    offset: u8,
    file: u8,
    line: u32,
}

#[derive(Arbitrary, Debug)]
struct InlineSeed {
    start: u8,
    width: u8,
    name: Vec<u8>,
    file: u8,
    line: u32,
}

#[derive(Arbitrary, Debug)]
struct CallSiteSeed {
    offset: u8,
    flags: u8,
    patterns: Vec<Vec<u8>>,
}

fuzz_target!(|case: Case| {
    let version = if case.control & 1 == 0 {
        GsymVersion::V1
    } else {
        GsymVersion::V2
    };
    let endian = if case.control & 2 == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let function_set = match case.control & 4 {
        0 => FunctionSetPolicy::Deduplicate,
        _ => FunctionSetPolicy::MergeEqualRanges,
    };
    let merge = matches!(function_set, FunctionSetPolicy::MergeEqualRanges);
    let repair = case.control & 8 != 0;
    let executable_end =
        BASE_ADDRESS + SLOTS * SLOT_SIZE + SLOT_SIZE + u64::from(case.executable_tail);

    let Model { files, functions } = model(&case);
    if functions.is_empty() {
        return;
    }
    let settings = Settings {
        version,
        endian,
        function_set,
        repair,
        executable_end,
    };
    let forward = encode(&files, functions.iter().cloned(), settings);
    let reverse = encode(&files, functions.iter().rev().cloned(), settings);
    assert_eq!(forward, reverse, "insertion order changed writer output");

    let parsed = Gsym::parse(forward).expect("writer output must parse");
    parsed.verify().expect("writer output must verify");
    let decoded = parsed.decode_all().expect("writer output must decode");
    assert!(!decoded.functions.is_empty());
    if merge && shares_a_range(&functions) {
        assert!(
            decoded
                .functions
                .iter()
                .any(|function| !function.merged.is_empty()),
            "merged-function mode discarded every equal range"
        );
    }
    for function in &decoded.functions {
        for address in [
            function.range.start,
            function.range.start + function.range.size() / 2,
            function.range.end,
        ] {
            drop(parsed.lookup(address).expect("valid lookup must not fail"));
        }
    }

    let transcoded = parsed
        .transcode(TranscodeOptions {
            version: Some(if version == GsymVersion::V1 {
                GsymVersion::V2
            } else {
                GsymVersion::V1
            }),
            endian: Some(match endian {
                Endian::Little => Endian::Big,
                Endian::Big => Endian::Little,
            }),
        })
        .expect("writer output must transcode");
    let transcoded = Gsym::parse(transcoded).expect("transcoded output must parse");
    transcoded.verify().expect("transcoded output must verify");
    assert_semantically_equal(&parsed, &transcoded, &decoded.functions);
});

fn assert_semantically_equal(
    source: &Gsym<Vec<u8>>,
    transcoded: &Gsym<Vec<u8>>,
    functions: &[Function],
) {
    assert_eq!(source.build_id(), transcoded.build_id());
    assert_eq!(
        source.header().base_address,
        transcoded.header().base_address
    );
    assert_eq!(
        source.header().address_count,
        transcoded.header().address_count
    );
    for function in functions {
        for address in function.range.start..=function.range.end {
            let source = source.lookup(address).expect("source lookup must succeed");
            let transcoded = transcoded
                .lookup(address)
                .expect("transcoded lookup must succeed");
            assert_eq!(source, transcoded, "lookup changed at {address:#x}");
        }
    }
}

/// The file table and the functions both insertion orders encode.
struct Model {
    files: Vec<FileEntry>,
    functions: Vec<Function>,
}

/// Builds the model both insertion orders encode.
fn model(case: &Case) -> Model {
    let mut names = HashSet::new();
    let mut seeds = Vec::new();
    for seed in case.functions.iter().take(MAX_FUNCTIONS) {
        let name = name(seed);
        if names.insert(name.clone()) {
            seeds.push((seed, u64::from(seed.slot) % SLOTS, name));
        }
    }
    let mut occupied = seeds.iter().map(|(_, slot, _)| *slot).collect::<Vec<_>>();
    occupied.sort_unstable();
    occupied.dedup();
    let files = occupied
        .iter()
        .enumerate()
        .map(|(position, slot)| file(case.files.get(position), *slot))
        .collect::<Vec<_>>();
    let indices = (0..=files.len())
        .map(|index| FileIndex::from(u32::try_from(index).unwrap_or(u32::MAX)))
        .collect::<Vec<_>>();

    let mut functions = Vec::with_capacity(seeds.len());
    for (seed, slot, name) in seeds {
        let own_file = occupied
            .iter()
            .position(|candidate| *candidate == slot)
            .map_or(FileIndex::ZERO, |position| {
                file_at(&indices, position.saturating_add(1))
            });
        let start = BASE_ADDRESS + slot * SLOT_SIZE;
        let width = u64::from(seed.width);
        let range = AddressRange::new(start, start + width);
        let mut lines = Vec::new();
        if width != 0 {
            lines.push(LineEntry::new(start, own_file, 1));
            lines.extend(seed.lines.iter().take(MAX_LINES).map(|line| {
                LineEntry::new(
                    start + u64::from(line.offset) % width,
                    file_at(&indices, usize::from(line.file)),
                    line.line,
                )
            }));
        }
        lines.sort_by_key(|line| (line.address, line.file, line.line));
        lines.dedup();
        functions.push(Function {
            lines,
            inline: inline_chain(&seed.inline, range, &indices),
            call_sites: seed
                .call_sites
                .iter()
                .take(MAX_CALL_SITES)
                .filter(|_| width != 0)
                .map(|site| CallSite {
                    return_offset: u64::from(site.offset) % width,
                    flags: CallSiteFlags::from_bits_retain(site.flags),
                    match_regex: site
                        .patterns
                        .iter()
                        .take(MAX_PATTERNS)
                        .map(|pattern| clean_bytes(pattern, b"callee.*"))
                        .collect(),
                })
                .collect(),
            ..Function::new(range, name)
        });
    }
    Model { files, functions }
}

/// Names the file interned for one address slot.
fn file(seed: Option<&FileSeed>, slot: u64) -> FileEntry {
    let (directory, basename) = seed.map_or_else(
        || (b"src".to_vec(), b"slot".to_vec()),
        |seed| {
            (
                clean_bytes(&seed.directory, b"src"),
                clean_bytes(&seed.basename, b"slot"),
            )
        },
    );
    let mut suffixed = basename;
    suffixed.extend_from_slice(format!("_{slot}.rs").as_bytes());
    FileEntry::new(directory, suffixed)
}

/// Renders a function name as a plain, Itanium or Swift symbol.
fn name(seed: &FunctionSeed) -> Vec<u8> {
    let identifier = clean_bytes(&seed.name, b"function");
    let length = identifier.len().to_string().into_bytes();
    match seed.style % 3 {
        0 => identifier,
        1 => {
            let mut name = b"_Z".to_vec();
            name.extend_from_slice(&length);
            name.extend_from_slice(&identifier);
            name.extend_from_slice(b"v");
            name
        }
        _ => {
            let mut name = b"$s4main".to_vec();
            name.extend_from_slice(&length);
            name.extend_from_slice(&identifier);
            name.extend_from_slice(b"yyF");
            name
        }
    }
}

fn inline_chain(
    seeds: &[InlineSeed],
    function: AddressRange,
    files: &[FileIndex],
) -> Option<InlineNode> {
    let mut parent = function;
    let mut descriptions = Vec::new();
    for (depth, seed) in seeds.iter().take(MAX_INLINE_DEPTH).enumerate() {
        let parent_width = parent.size();
        if parent_width == 0 {
            break;
        }
        let offset = u64::from(seed.start) % parent_width;
        let available = parent_width - offset;
        let width = 1 + u64::from(seed.width) % available;
        let range = AddressRange::new(parent.start + offset, parent.start + offset + width);
        descriptions.push((
            range,
            clean_bytes(&seed.name, format!("inline_{depth}").as_bytes()),
            file_at(files, usize::from(seed.file)),
            seed.line,
        ));
        parent = range;
    }
    let mut child = None;
    for (range, name, call_file, call_line) in descriptions.into_iter().rev() {
        child = Some(InlineNode {
            ranges: vec![range],
            name,
            call_file,
            call_line,
            children: child.into_iter().collect(),
        });
    }
    child
}

fn shares_a_range(functions: &[Function]) -> bool {
    functions.iter().enumerate().any(|(index, function)| {
        functions
            .get(index.saturating_add(1)..)
            .unwrap_or_default()
            .iter()
            .any(|other| other.range == function.range)
    })
}

fn clean_bytes(bytes: &[u8], fallback: &[u8]) -> Vec<u8> {
    let mut bytes = bytes
        .iter()
        .take(MAX_STRING)
        .map(|byte| if *byte == 0 { b'_' } else { *byte })
        .collect::<Vec<_>>();
    if bytes.is_empty() {
        bytes.extend_from_slice(fallback);
    }
    bytes
}

const fn file_at(files: &[FileIndex], index: usize) -> FileIndex {
    if files.is_empty() {
        FileIndex::ZERO
    } else {
        files[index % files.len()]
    }
}

#[derive(Clone, Copy)]
struct Settings {
    version: GsymVersion,
    endian: Endian,
    function_set: FunctionSetPolicy,
    repair: bool,
    executable_end: u64,
}

fn encode(
    files: &[FileEntry],
    functions: impl IntoIterator<Item = Function>,
    settings: Settings,
) -> Vec<u8> {
    let mut builder = GsymBuilder::new()
        .version(settings.version)
        .endian(settings.endian)
        .base_address(BASE_ADDRESS)
        .build_id([0x12, 0x34, 0x56, 0x78])
        .repair_zero_sized_functions(settings.repair)
        .function_set(settings.function_set)
        .executable_ranges([AddressRange::new(BASE_ADDRESS, settings.executable_end)]);
    for file in files {
        builder
            .add_file(file.clone())
            .expect("valid file entry must intern");
    }
    for function in functions {
        builder
            .add_function(function)
            .expect("valid function must be accepted");
    }
    builder.to_bytes().expect("valid model must encode")
}

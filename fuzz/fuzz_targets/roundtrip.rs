#![no_main]

use arbitrary::Arbitrary;
use gsym::{
    AddressRange, CallSite, CallSiteFlags, Endian, FileEntry, FileIndex, FrameLookupOptions,
    Function, Gsym, GsymBuilder, GsymVersion, InlineNode, LineEntry, LookupScratch,
    TranscodeOptions,
};
use libfuzzer_sys::fuzz_target;

const MAX_FILES: usize = 8;
const MAX_FUNCTIONS: usize = 16;
const MAX_LINES: usize = 24;
const MAX_INLINE_DEPTH: usize = 8;
const MAX_ALIASES: usize = 4;
const MAX_CALL_SITES: usize = 8;
const MAX_PATTERNS: usize = 4;
const MAX_STRING: usize = 48;

#[derive(Arbitrary, Debug)]
struct Case {
    control: u8,
    build_id: Vec<u8>,
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
    width: u16,
    gap: u16,
    name: Vec<u8>,
    lines: Vec<LineSeed>,
    inline: Vec<InlineSeed>,
    aliases: Vec<Vec<u8>>,
    call_sites: Vec<CallSiteSeed>,
}

#[derive(Arbitrary, Debug)]
struct LineSeed {
    offset: u16,
    file: u8,
    line: u32,
}

#[derive(Arbitrary, Debug)]
struct InlineSeed {
    start: u16,
    width: u16,
    name: Vec<u8>,
    file: u8,
    line: u32,
}

#[derive(Arbitrary, Debug)]
struct CallSiteSeed {
    offset: u16,
    flags: u8,
    patterns: Vec<Vec<u8>>,
}

fuzz_target!(|case: Case| {
    let Some(original) = encode(&case) else {
        return;
    };
    let repeated = encode(&case).expect("the same semantic case must remain encodable");
    assert_eq!(original, repeated, "writer output is not deterministic");

    let parsed = Gsym::parse(original.as_slice()).expect("writer output must parse");
    parsed.verify().expect("writer output must verify");
    let first_model = parsed
        .decode_all()
        .expect("writer output must fully decode");

    let target = TranscodeOptions {
        version: Some(if parsed.header().version == GsymVersion::V1 {
            GsymVersion::V2
        } else {
            GsymVersion::V1
        }),
        endian: Some(match parsed.header().endian {
            Endian::Little => Endian::Big,
            Endian::Big => Endian::Little,
        }),
    };
    let transformed = parsed
        .transcode(target)
        .expect("valid compatible data must transcode");
    let transformed = Gsym::parse(transformed).expect("transcoded output must parse");
    transformed.verify().expect("transcoded output must verify");
    let second_model = transformed
        .decode_all()
        .expect("transcoded output must fully decode");
    assert_semantics(&first_model, &second_model);

    for function in first_model.functions.iter().take(MAX_FUNCTIONS) {
        let addresses = [
            function.range.start,
            function.range.start + function.range.size() / 2,
            function.range.end.saturating_sub(1),
        ];
        for address in addresses {
            assert_eq!(
                parsed
                    .lookup(address)
                    .expect("original lookup must not fail"),
                transformed
                    .lookup(address)
                    .expect("transcoded lookup must not fail"),
                "transcoding changed address lookup"
            );
            assert_visitor_matches(&parsed, address);
            assert_visitor_matches(&transformed, address);
        }
    }

    if case.control & 4 != 0 {
        let segments = first_model
            .segments(2048, target)
            .expect("valid decoded data must segment");
        assert!(!segments.is_empty());
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.function_count)
                .sum::<usize>(),
            first_model.functions.len()
        );
        for segment in segments {
            Gsym::parse(segment.into_bytes())
                .and_then(|gsym| gsym.verify())
                .expect("every segment must independently verify");
        }
    }
});

fn encode(case: &Case) -> Option<Vec<u8>> {
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
    let function_count = case.functions.len().min(MAX_FUNCTIONS);
    if function_count == 0 {
        return None;
    }
    let mut build_id = clean_bytes(&case.build_id, b"build-id");
    build_id.truncate(20);
    let mut builder = GsymBuilder::new()
        .version(version)
        .endian(endian)
        .base_address(0x10_0000)
        .build_id(build_id)
        .repair_zero_sized_functions(false);

    let file_count = case.files.len().min(MAX_FILES).min(function_count);
    let mut files = Vec::with_capacity(file_count);
    for (index, seed) in case.files.iter().take(file_count).enumerate() {
        let directory = clean_bytes(&seed.directory, b"src");
        let basename = clean_bytes(&seed.basename, format!("file{index}.rs").as_bytes());
        files.push(builder.add_file(FileEntry::new(directory, basename)).ok()?);
    }

    let mut start = 0x10_0000_u64;
    for (index, seed) in case.functions.iter().take(function_count).enumerate() {
        start = start.checked_add(u64::from(seed.gap % 64))?;
        let width = u64::from(seed.width).max(1);
        let range = AddressRange::new(start, start + width);
        let assigned_file = file_at(&files, index);
        let mut lines = vec![LineEntry::new(range.start, assigned_file, 1)];
        lines.extend(seed.lines.iter().take(MAX_LINES).map(|line| {
            LineEntry::new(
                range.start + u64::from(line.offset) % width,
                file_at(&files, usize::from(line.file)),
                line.line,
            )
        }));
        lines.sort_by_key(|line| (line.address, line.file, line.line));
        lines.dedup();

        let inline = inline_chain(&seed.inline, range, &files);
        let merged = seed
            .aliases
            .iter()
            .take(MAX_ALIASES)
            .enumerate()
            .map(|(alias, name)| {
                Function::new(
                    range,
                    clean_bytes(name, format!("alias_{index}_{alias}").as_bytes()),
                )
            })
            .collect();
        let call_sites = seed
            .call_sites
            .iter()
            .take(MAX_CALL_SITES)
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
            .collect();
        builder
            .add_function(Function {
                lines,
                inline,
                merged,
                call_sites,
                ..Function::new(
                    range,
                    clean_bytes(&seed.name, format!("function_{index}").as_bytes()),
                )
            })
            .ok()?;
        start = range.end.checked_add(0x100)?;
    }
    builder.to_bytes().ok()
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

fn file_at(files: &[FileIndex], index: usize) -> FileIndex {
    if files.is_empty() {
        FileIndex::ZERO
    } else {
        files[index % files.len()]
    }
}

fn assert_semantics(left: &gsym::DecodedGsym, right: &gsym::DecodedGsym) {
    assert_eq!(left.base_address, right.base_address);
    assert_eq!(left.build_id, right.build_id);
    assert_eq!(left.files, right.files);
    assert_eq!(left.functions, right.functions);
}

fn assert_visitor_matches<D: AsRef<[u8]>>(gsym: &Gsym<D>, address: u64) {
    let expected = gsym.lookup(address).expect("lookup must not fail");
    let mut scratch = LookupScratch::with_capacity(MAX_INLINE_DEPTH);
    let mut frames = Vec::new();
    let found = gsym
        .for_each_frame(
            address,
            FrameLookupOptions::default(),
            &mut scratch,
            |frame| frames.push(frame),
        )
        .expect("frame visitation must not fail");
    assert_eq!(found, expected.is_some());
    if let Some(expected) = expected {
        assert_eq!(frames.as_slice(), expected.frames());
    }
}

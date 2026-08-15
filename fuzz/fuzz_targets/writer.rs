#![no_main]

use gsym::{
    AddressRange, CallSite, CallSiteFlags, Endian, FileEntry, Function, Gsym, GsymBuilder,
    GsymVersion, InlineNode, LineEntry, TranscodeOptions,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let control = data.first().copied().unwrap_or(0);
    let version = if control & 1 == 0 {
        GsymVersion::V1
    } else {
        GsymVersion::V2
    };
    let endian = if control & 2 == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let merge = control & 4 != 0;
    let repair = control & 8 != 0;
    let width = 16 + u64::from(data.get(1).copied().unwrap_or(0));
    let executable_end = 0x3000 + u64::from(data.get(2).copied().unwrap_or(0)).max(1);

    let functions = functions(width, data);
    let forward = encode(
        functions.iter().cloned(),
        version,
        endian,
        merge,
        repair,
        executable_end,
    );
    let reverse = encode(
        functions.iter().rev().cloned(),
        version,
        endian,
        merge,
        repair,
        executable_end,
    );
    assert_eq!(forward, reverse, "insertion order changed writer output");

    let parsed = Gsym::parse(forward).expect("writer output must parse");
    parsed.verify().expect("writer output must verify");
    let decoded = parsed.decode_all().expect("writer output must decode");
    assert!(!decoded.functions.is_empty());
    if merge {
        assert!(
            decoded
                .functions
                .iter()
                .any(|function| !function.merged.is_empty()),
            "merged-function mode discarded every equal range"
        );
    }
    for address in [0x1010, 0x1010 + width / 2, 0x3000] {
        drop(parsed.lookup(address).expect("valid lookup must not fail"));
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
    assert_eq!(
        decoded.functions,
        transcoded
            .decode_all()
            .expect("transcoded output must decode")
            .functions
    );
});

fn functions(width: u64, data: &[u8]) -> Vec<Function> {
    let range = AddressRange::new(0x1010, 0x1010 + width);
    let line = 1 + u32::from(data.get(3).copied().unwrap_or(0));
    let flags = CallSiteFlags::from_bits_retain(data.get(4).copied().unwrap_or(0));
    let rich = Function {
        lines: vec![LineEntry::new(range.start, 1_u32.into(), line)],
        inline: Some(InlineNode {
            ranges: vec![AddressRange::new(range.start + 1, range.end)],
            name: b"inline_rich".to_vec(),
            call_file: 1_u32.into(),
            call_line: line,
            children: Vec::new(),
        }),
        call_sites: vec![CallSite {
            return_offset: width / 2,
            flags,
            match_regex: vec![b"callee.*".to_vec()],
        }],
        ..Function::new(range, b"foo")
    };
    vec![
        Function::new(range, b"_Z3foov"),
        rich,
        Function::new(range, b"range_alias"),
        Function::new(AddressRange::new(0x2000, 0x2020), b"middle"),
        Function::new(AddressRange::new(0x3000, 0x3000), b"zero_a"),
        Function::new(AddressRange::new(0x3000, 0x3000), b"zero_b"),
    ]
}

fn encode(
    functions: impl IntoIterator<Item = Function>,
    version: GsymVersion,
    endian: Endian,
    merge: bool,
    repair: bool,
    executable_end: u64,
) -> Vec<u8> {
    let mut builder = GsymBuilder::new()
        .version(version)
        .endian(endian)
        .base_address(0x1000)
        .build_id([0x12, 0x34, 0x56, 0x78])
        .merge_equal_address_functions(merge)
        .repair_zero_sized_functions(repair)
        .executable_ranges([
            AddressRange::new(0x1000, 0x2100),
            AddressRange::new(0x3000, executable_end),
        ]);
    builder
        .add_file(FileEntry::new(b"src", b"writer.rs"))
        .unwrap();
    for function in functions {
        builder.add_function(function).unwrap();
    }
    builder.to_bytes().expect("valid model must encode")
}

//! Address lookup semantics, from range boundaries to inline frame stacks.

use gsym::{Gsym, GsymVersion};

use crate::bytes::ByteOrder;
use crate::fixture::{
    RawFunction, basic_functions, build, call_sites, inline_info, line_table, merged,
    string_offsets,
};
use crate::minimal::minimal_v1;
use crate::name_at;

#[test]
fn lookup_obeys_half_open_function_ranges() {
    let bytes = minimal_v1(ByteOrder::Little);
    let gsym = Gsym::parse(&bytes).unwrap();

    assert!(gsym.lookup(0x0fff).unwrap().is_none());
    assert_eq!(name_at(&gsym, 0x1000), b"alpha");
    assert_eq!(name_at(&gsym, 0x100f), b"alpha");
    assert!(gsym.lookup(0x1010).unwrap().is_none());
}

#[test]
fn boundary_gap_and_zero_size_cases_v1_v2_both_endians() {
    let offsets = string_offsets();
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let fixture = build(version, order, &basic_functions(offsets), &[]);
            let reader = Gsym::parse(&fixture.bytes).unwrap();
            assert!(reader.lookup(0x0fff).unwrap().is_none());
            assert_eq!(name_at(&reader, 0x1000), b"alpha");
            assert_eq!(name_at(&reader, 0x100f), b"alpha");
            assert!(reader.lookup(0x1010).unwrap().is_none());
            assert!(reader.lookup(0x101f).unwrap().is_none());
            assert_eq!(name_at(&reader, 0x1020), b"beta");
            assert_eq!(name_at(&reader, 0xffff), b"beta");
            reader.verify().unwrap();
        }
    }
}

#[test]
fn equal_start_and_overlapping_unequal_ranges_choose_first_containing() {
    let offsets = string_offsets();
    let functions = vec![
        RawFunction {
            address: 0x1000,
            size: 0x50,
            name: offsets.alpha,
            records: Vec::new(),
        },
        RawFunction {
            address: 0x1000,
            size: 0x100,
            name: offsets.beta,
            records: Vec::new(),
        },
        RawFunction {
            address: 0x1080,
            size: 0x100,
            name: offsets.alias,
            records: Vec::new(),
        },
    ];
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let fixture = build(version, order, &functions, &[]);
            let reader = Gsym::parse(&fixture.bytes).unwrap();
            assert_eq!(name_at(&reader, 0x104f), b"alpha");
            assert_eq!(name_at(&reader, 0x1050), b"beta");
            assert_eq!(name_at(&reader, 0x107f), b"beta");
            assert_eq!(name_at(&reader, 0x1080), b"alias");
            reader.verify().unwrap();
        }
    }
}

#[test]
fn unknown_tlv_is_skipped_by_lookup() {
    let offsets = string_offsets();
    let functions = [RawFunction {
        address: 0x1000,
        size: 0x10,
        name: offsets.alpha,
        records: vec![(0xfeed_beef, vec![1, 2, 3, 4, 5])],
    }];
    for version in [GsymVersion::V1, GsymVersion::V2] {
        let fixture = build(version, ByteOrder::Little, &functions, &[]);
        let reader = Gsym::parse(&fixture.bytes).unwrap();
        assert_eq!(name_at(&reader, 0x1004), b"alpha");
    }
}

#[test]
fn rich_inline_callsite_and_merged_lookup_v1_v2_both_endians() {
    let offsets = string_offsets();
    let files = [
        (offsets.empty, offsets.empty),
        (offsets.tmp, offsets.main_c),
        (offsets.tmp, offsets.foo_h),
    ];
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let function = RawFunction {
                address: 0x1000,
                size: 0x100,
                name: offsets.main,
                records: vec![
                    (1, line_table()),
                    (2, inline_info(offsets, version, order)),
                    (3, merged(offsets, version, order)),
                    (4, call_sites(offsets, version, order)),
                ],
            };
            let fixture = build(version, order, &[function], &files);
            let reader = Gsym::parse(&fixture.bytes).unwrap();
            let lookup = reader.lookup(0x1012).unwrap().unwrap();
            assert_eq!(
                lookup
                    .frames()
                    .iter()
                    .map(|frame| frame.name)
                    .collect::<Vec<_>>(),
                vec![b"inline2".as_slice(), b"inline1", b"main"]
            );
            assert_eq!(lookup.frames()[0].line, 20);
            assert_eq!(lookup.frames()[0].basename, b"foo.h");
            assert_eq!(lookup.frames()[1].line, 33);
            assert_eq!(lookup.frames()[1].basename, b"foo.h");
            assert_eq!(lookup.frames()[2].line, 6);
            assert_eq!(lookup.frames()[2].basename, b"main.c");
            assert_eq!(lookup.call_site_patterns(), [b"^callee$".as_slice()]);
            let decoded = reader.function(0).unwrap().decode().unwrap();
            assert_eq!(decoded.merged.len(), 1);
            assert_eq!(decoded.merged[0].name, b"alias");
            reader.verify().unwrap();
        }
    }
}

//! Corrupt, truncated and arbitrary inputs must fail rather than panic.

use gsym::{Gsym, GsymVersion};
use proptest::prelude::*;

use crate::bytes::{ByteOrder, as_u64, patch_uint};
use crate::fixture::{RawFunction, build, inline_info, line_table, offset_width, string_offsets};
use crate::minimal::{minimal_v1, minimal_v2};

#[test]
fn rejects_empty_and_short_inputs() {
    for length in 0..48 {
        let error = Gsym::parse(&vec![0; length]).unwrap_err();
        if length < 4 {
            assert!(matches!(
                error,
                gsym::Error::UnexpectedEof {
                    offset: 0,
                    needed: 4,
                    remaining,
                } if remaining == length
            ));
        } else {
            assert!(matches!(error, gsym::Error::InvalidMagic(0)));
        }
    }
}

#[test]
fn rejects_bad_magic_version_and_address_width() {
    let mut bytes = minimal_v1(ByteOrder::Little);
    bytes[0] ^= 0xff;
    assert!(matches!(
        Gsym::parse(&bytes),
        Err(gsym::Error::InvalidMagic(_))
    ));

    let mut bytes = minimal_v1(ByteOrder::Little);
    bytes[4..6].copy_from_slice(&99_u16.to_le_bytes());
    assert!(matches!(
        Gsym::parse(&bytes),
        Err(gsym::Error::UnsupportedVersion(99))
    ));

    let mut bytes = minimal_v1(ByteOrder::Little);
    bytes[6] = 3;
    assert!(matches!(
        Gsym::parse(&bytes),
        Err(gsym::Error::InvalidAddressOffsetSize {
            version: 1,
            size: 3
        })
    ));
}

#[test]
fn rejects_v1_string_tables_outside_the_file() {
    let mut bytes = minimal_v1(ByteOrder::Little);
    bytes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Gsym::parse(&bytes).is_err());
}

#[test]
fn rejects_v2_sections_outside_the_file() {
    let mut bytes = minimal_v2(ByteOrder::Little);
    // First GlobalData entry starts at byte 20; its 64-bit file offset starts
    // at byte 24.
    bytes[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(Gsym::parse(&bytes).is_err());
}

#[test]
fn function_iteration_reports_malformed_records_without_losing_its_length() {
    let mut bytes = minimal_v1(ByteOrder::Little);
    bytes[56..60].copy_from_slice(&u32::MAX.to_le_bytes());
    let gsym = Gsym::parse(&bytes).unwrap();
    let mut functions = gsym.functions();
    assert_eq!(functions.len(), 2);
    assert_eq!(functions.next().unwrap().unwrap().name(), b"alpha");
    assert_eq!(functions.len(), 1);
    assert!(matches!(
        functions.next().unwrap(),
        Err(gsym::Error::InvalidOffset {
            offset,
            ..
        }) if offset == u64::from(u32::MAX)
    ));
    assert_eq!(functions.len(), 0);
    assert!(functions.next().is_none());
}

#[test]
fn every_truncation_of_valid_files_is_rejected_without_panicking() {
    for bytes in [
        minimal_v1(ByteOrder::Little),
        minimal_v1(ByteOrder::Big),
        minimal_v2(ByteOrder::Little),
        minimal_v2(ByteOrder::Big),
    ] {
        for length in 0..bytes.len() {
            if let Ok(gsym) = Gsym::parse(&bytes[..length]) {
                assert!(
                    gsym.verify().is_err(),
                    "accepted and verified {length}/{} bytes",
                    bytes.len()
                );
            }
        }
    }
}

#[test]
fn invalid_function_offsets_strings_and_files_fail_verification() {
    let offsets = string_offsets();
    for version in [GsymVersion::V1, GsymVersion::V2] {
        let width = offset_width(version);
        let function = RawFunction {
            address: 0x1000,
            size: 0x20,
            name: offsets.alpha,
            records: vec![(1, line_table())],
        };
        let files = [(offsets.empty, offsets.empty)];
        let fixture = || {
            build(
                version,
                ByteOrder::Little,
                std::slice::from_ref(&function),
                &files,
            )
        };

        let mut bad_offset = fixture();
        for slot in &bad_offset.address_info_slots {
            patch_uint(
                &mut bad_offset.bytes,
                *slot,
                u64::MAX,
                width,
                ByteOrder::Little,
            );
        }
        let parsed = Gsym::parse(&bad_offset.bytes).unwrap();
        assert!(parsed.verify().is_err());

        let mut bad_string = fixture();
        patch_uint(
            &mut bad_string.bytes,
            bad_string.name_slots[0],
            as_u64(bad_string.string_range.len()) + 1,
            width,
            ByteOrder::Little,
        );
        let parsed = Gsym::parse(&bad_string.bytes).unwrap();
        assert!(parsed.verify().is_err());

        let mut invalid_line = line_table();
        invalid_line[4] = 9;
        let invalid_function = RawFunction {
            records: vec![(1, invalid_line)],
            ..function
        };
        let invalid_fixture = build(version, ByteOrder::Little, &[invalid_function], &files);
        let parsed = Gsym::parse(&invalid_fixture.bytes).unwrap();
        assert!(parsed.lookup(0x1012).is_err());
        assert!(parsed.verify().is_err());
    }
}

#[test]
fn lookup_defers_full_inline_range_validation_to_verify() {
    let offsets = string_offsets();
    let files = [
        (offsets.empty, offsets.empty),
        (offsets.tmp, offsets.main_c),
        (offsets.tmp, offsets.foo_h),
    ];
    for version in [GsymVersion::V1, GsymVersion::V2] {
        let function = RawFunction {
            address: 0x1000,
            // The encoded inline root extends to 0x1100. Lookup only decodes
            // the matching path; whole-file verification checks containment.
            size: 0x20,
            name: offsets.main,
            records: vec![(2, inline_info(offsets, version, ByteOrder::Little))],
        };
        let fixture = build(version, ByteOrder::Little, &[function], &files);
        let reader = Gsym::parse(&fixture.bytes).unwrap();

        let hit = reader.lookup(0x1012).unwrap().unwrap();
        assert_eq!(hit.frames().first().unwrap().name, b"inline2");
        assert!(reader.verify().is_err());
    }
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        drop(Gsym::parse(&data).and_then(|gsym| gsym.verify().map(|_| ())));
    }
}

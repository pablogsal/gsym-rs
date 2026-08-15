use crate::endian::{Cursor, Encoder, Endian};
use crate::error::Error;
use crate::format::function::{self, EncodedCallSite, EncodedFunction, EncodedInlineNode};
use crate::format::{INFO_CALL_SITE, INFO_END, INFO_INLINE, INFO_LINE_TABLE, INFO_MERGED};
use crate::model::{AddressRange, LineEntry};

use super::ENDIANS;

const STRING_WIDTHS: [u8; 2] = [4, 8];

fn minimal_function(name: u64) -> EncodedFunction {
    EncodedFunction {
        range: AddressRange::new(0x1000, 0x1100),
        name,
        ..EncodedFunction::default()
    }
}

fn nested_inline(root_name: u64, child_name: u64) -> EncodedInlineNode {
    EncodedInlineNode {
        ranges: vec![AddressRange::new(0x1000, 0x1100)],
        name: root_name,
        call_file: 0,
        call_line: 0,
        children: vec![EncodedInlineNode {
            ranges: vec![AddressRange::new(0x1010, 0x1060)],
            name: child_name,
            call_file: 2,
            call_line: 22,
            children: vec![
                EncodedInlineNode {
                    ranges: vec![AddressRange::new(0x1012, 0x1015)],
                    name: child_name.saturating_add(1),
                    call_file: 3,
                    call_line: 33,
                    children: Vec::new(),
                },
                EncodedInlineNode {
                    ranges: vec![AddressRange::new(0x1057, 0x1058)],
                    name: child_name.saturating_add(2),
                    call_file: 4,
                    call_line: 44,
                    children: Vec::new(),
                },
            ],
        }],
    }
}

fn rich_function(width: u8) -> EncodedFunction {
    let large = if width == 8 { 0x1_0000_0001 } else { 1 };
    EncodedFunction {
        range: AddressRange::new(0x1000, 0x1200),
        name: large,
        lines: Some(vec![
            LineEntry {
                address: 0x1000,
                file: 1.into(),
                line: 10,
            },
            LineEntry {
                address: 0x1010,
                file: 1.into(),
                line: 11,
            },
            LineEntry {
                address: 0x1100,
                file: 2.into(),
                line: 1000,
            },
        ]),
        inline: Some(nested_inline(
            large.saturating_add(1),
            large.saturating_add(2),
        )),
        merged: vec![EncodedFunction {
            range: AddressRange::new(0x1000, 0x1200),
            name: large.saturating_add(10),
            ..EncodedFunction::default()
        }],
        call_sites: vec![EncodedCallSite {
            return_offset: 0x30,
            flags: 1,
            match_regex: vec![large.saturating_add(20), large.saturating_add(21)],
        }],
    }
}

#[test]
fn function_info_round_trips_every_optional_record() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            let expected = rich_function(width);
            let bytes = function::encode(&expected, endian, width).unwrap();
            let decoded =
                function::decode_exact(&bytes, endian, width, expected.range.start).unwrap();
            assert_eq!(decoded, expected);
        }
    }
}

#[test]
fn function_and_all_nested_records_preserve_large_v2_offsets() {
    let expected = rich_function(8);
    let bytes = function::encode(&expected, Endian::Little, 8).unwrap();
    let decoded = function::decode_exact(&bytes, Endian::Little, 8, 0x1000).unwrap();
    assert!(decoded.name > u64::from(u32::MAX));
    assert!(decoded.inline.unwrap().name > u64::from(u32::MAX));
    assert!(decoded.merged.first().unwrap().name > u64::from(u32::MAX));
    assert!(
        decoded
            .call_sites
            .first()
            .unwrap()
            .match_regex
            .iter()
            .all(|offset| *offset > u64::from(u32::MAX))
    );
}

#[test]
fn function_info_matches_reference_minimal_bytes() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            let bytes = function::encode(&minimal_function(1), endian, width).unwrap();
            assert_eq!(bytes.len(), 4 + usize::from(width) + 8);
            let mut cursor = Cursor::new(&bytes, endian);
            assert_eq!(cursor.read_u32().unwrap(), 0x100);
            assert_eq!(cursor.read_uint(width).unwrap(), 1);
            assert_eq!(cursor.read_u32().unwrap(), INFO_END);
            assert_eq!(cursor.read_u32().unwrap(), 0);
        }
    }
}

#[test]
fn function_info_reports_reference_decode_and_encode_errors() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            let mut output = Encoder::new(endian);
            assert!(function::decode(output.as_slice(), endian, width, 0x1000).is_err());
            output.write_u32(0x100);
            assert!(function::decode(output.as_slice(), endian, width, 0x1000).is_err());
            output.write_uint(0, width).unwrap();
            assert!(matches!(
                function::decode(output.as_slice(), endian, width, 0x1000),
                Err(Error::ZeroNameOffset)
            ));
            output.patch_uint(4, 1, width).unwrap();
            assert!(function::decode(output.as_slice(), endian, width, 0x1000).is_err());
            output.write_u32(7);
            output.write_u32(0);
            assert!(matches!(
                function::decode(output.as_slice(), endian, width, 0x1000),
                Err(Error::UnsupportedInfoType(7))
            ));

            let mut invalid = minimal_function(0);
            assert!(matches!(
                function::encode(&invalid, endian, width),
                Err(Error::ZeroNameOffset)
            ));
            invalid.name = 1;
            invalid.range.end = invalid.range.start + u64::from(u32::MAX) + 1;
            assert!(function::encode(&invalid, endian, width).is_err());
            assert!(function::encode(&minimal_function(1), endian, 3).is_err());
        }
    }
}

#[test]
fn function_info_rejects_truncated_payloads_unknown_types_and_bad_end_markers() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            let bytes = function::encode(&rich_function(width), endian, width).unwrap();
            for length in 0..bytes.len() {
                assert!(
                    function::decode_exact(bytes.get(..length).unwrap(), endian, width, 0x1000)
                        .is_err()
                );
            }

            let mut trailing = function::encode(&minimal_function(1), endian, width).unwrap();
            trailing.push(0);
            assert!(function::decode_exact(&trailing, endian, width, 0x1000).is_err());

            let mut bad_end = Encoder::new(endian);
            bad_end.write_u32(0x100);
            bad_end.write_uint(1, width).unwrap();
            bad_end.write_u32(INFO_END);
            bad_end.write_u32(1);
            bad_end.write_u8(0);
            assert!(function::decode(bad_end.as_slice(), endian, width, 0x1000).is_err());

            let mut truncated_record = Encoder::new(endian);
            truncated_record.write_u32(0x100);
            truncated_record.write_uint(1, width).unwrap();
            truncated_record.write_u32(INFO_LINE_TABLE);
            truncated_record.write_u32(10);
            truncated_record.write_u8(0);
            assert!(function::decode(truncated_record.as_slice(), endian, width, 0x1000).is_err());
        }
    }
}

#[test]
fn function_info_rejects_duplicate_empty_collection_records() {
    for endian in ENDIANS {
        for info_type in [INFO_MERGED, INFO_CALL_SITE] {
            let mut bytes = Encoder::new(endian);
            bytes.write_u32(0x100);
            bytes.write_u32(1);
            for _ in 0..2 {
                bytes.write_u32(info_type);
                bytes.write_u32(4);
                bytes.write_u32(0);
            }
            bytes.write_u32(INFO_END);
            bytes.write_u32(0);
            assert!(matches!(
                function::decode_exact(bytes.as_slice(), endian, 4, 0x1000),
                Err(Error::InvalidFormat(_))
            ));
        }
    }
}

#[test]
fn inline_ranges_and_sibling_terminators_match_llvm() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            let mut function_value = minimal_function(1);
            function_value.inline =
                Some(nested_inline(0, if width == 8 { 0x1_0000_0001 } else { 2 }));
            let bytes = function::encode(&function_value, endian, width).unwrap();
            let decoded = function::decode_exact(&bytes, endian, width, 0x1000).unwrap();
            assert_eq!(decoded, function_value);
        }
    }
}

#[test]
fn inline_address_ranges_handle_zero_one_and_many() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            for ranges in [
                vec![AddressRange::new(0x1000, 0x1010)],
                vec![
                    AddressRange::new(0x1000, 0x1010),
                    AddressRange::new(0x1020, 0x1030),
                    AddressRange::new(0x1050, 0x1070),
                ],
            ] {
                let mut expected = minimal_function(1);
                expected.inline = Some(EncodedInlineNode {
                    ranges,
                    ..EncodedInlineNode::default()
                });
                let bytes = function::encode(&expected, endian, width).unwrap();
                assert_eq!(
                    function::decode_exact(&bytes, endian, width, 0x1000).unwrap(),
                    expected
                );
            }

            let mut empty = minimal_function(1);
            empty.inline = Some(EncodedInlineNode::default());
            assert!(function::encode(&empty, endian, width).is_err());
        }
    }
}

#[test]
fn inline_encoding_rejects_empty_uncontained_unsorted_and_truncated_trees() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            let mut function_value = minimal_function(1);
            function_value.inline = Some(EncodedInlineNode::default());
            assert!(function::encode(&function_value, endian, width).is_err());

            function_value.inline = Some(EncodedInlineNode {
                ranges: vec![AddressRange::new(0x1000, 0x1100)],
                name: 0,
                call_file: 0,
                call_line: 0,
                children: vec![EncodedInlineNode {
                    ranges: vec![AddressRange::new(0x1100, 0x1200)],
                    name: 2,
                    call_file: 1,
                    call_line: 1,
                    children: Vec::new(),
                }],
            });
            assert!(function::encode(&function_value, endian, width).is_err());

            function_value.inline = Some(EncodedInlineNode {
                ranges: vec![
                    AddressRange::new(0x1080, 0x1090),
                    AddressRange::new(0x1070, 0x1078),
                ],
                name: 0,
                call_file: 0,
                call_line: 0,
                children: Vec::new(),
            });
            assert!(function::encode(&function_value, endian, width).is_err());

            let valid = rich_function(width);
            let bytes = function::encode(&valid, endian, width).unwrap();
            for length in 0..bytes.len() {
                assert!(
                    function::decode_exact(bytes.get(..length).unwrap(), endian, width, 0x1000)
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn merged_and_callsite_collections_round_trip_and_reject_truncation() {
    for endian in ENDIANS {
        for width in STRING_WIDTHS {
            let expected = rich_function(width);
            let bytes = function::encode(&expected, endian, width).unwrap();
            let actual = function::decode_exact(&bytes, endian, width, 0x1000).unwrap();
            assert_eq!(actual.merged, expected.merged);
            assert_eq!(actual.call_sites, expected.call_sites);
            let call_site = actual.call_sites.first().unwrap();
            assert_eq!(call_site.return_offset, 0x30);
            assert_eq!(call_site.flags, 1);
            assert_eq!(call_site.match_regex.len(), 2);
        }
    }
}

#[test]
fn record_type_headers_are_endian_aware() {
    for endian in ENDIANS {
        let mut value = minimal_function(1);
        value.inline = Some(EncodedInlineNode {
            ranges: vec![value.range],
            ..EncodedInlineNode::default()
        });
        let bytes = function::encode(&value, endian, 4).unwrap();
        let mut cursor = Cursor::new(&bytes, endian);
        cursor.take(8).unwrap();
        assert_eq!(cursor.read_u32().unwrap(), INFO_INLINE);
        let payload_size = cursor.read_u32().unwrap();
        assert!(payload_size > 0);
    }
}

use crate::endian::{Encoder, Endian};
use crate::error::Error;
use crate::format::{GSYM_MAGIC, v1, v2};

use super::{ENDIANS, patch_byte, patch_bytes};

#[test]
fn v1_headers_match_reference_fields_endianness_and_errors() {
    let header = v1::Header {
        address_offset_size: 4,
        base_address: 0x1000,
        address_count: 1,
        string_table_offset: 0x2000,
        string_table_size: 0x1000,
        uuid: v1::FixedUuid::new(&(0_u8..16).collect::<Vec<_>>()).unwrap(),
    };
    for endian in ENDIANS {
        let bytes = header.encode(endian).unwrap();
        assert_eq!(bytes.len(), v1::HEADER_SIZE);
        assert_eq!(
            bytes.split_at(4).0,
            if endian == Endian::Little {
                b"MYSG"
            } else {
                b"GSYM"
            }
        );
        assert_eq!(
            v1::Header::decode(&bytes).unwrap(),
            (header.clone(), endian)
        );
    }

    let mut invalid = header.clone();
    invalid.address_offset_size = 3;
    assert!(matches!(
        invalid.encode(Endian::Little),
        Err(Error::InvalidAddressOffsetSize {
            version: 1,
            size: 3
        })
    ));
    assert!(matches!(
        v1::FixedUuid::new(&[0; v1::MAX_UUID_SIZE + 1]),
        Err(Error::V1BuildIdTooLong { size: 21 })
    ));

    for length in 0..v1::HEADER_SIZE {
        let encoded = header.encode(Endian::Little).unwrap();
        assert!(v1::Header::decode(encoded.split_at(length).0).is_err());
    }
    let mut bad_version = header.encode(Endian::Little).unwrap();
    patch_bytes(&mut bad_version, 4, &12_u16.to_le_bytes());
    assert!(matches!(
        v1::Header::decode(&bad_version),
        Err(Error::UnsupportedVersion(12))
    ));

    let mut bad_magic = header.encode(Endian::Little).unwrap();
    patch_bytes(&mut bad_magic, 0, &12_u32.to_le_bytes());
    assert!(matches!(
        v1::Header::decode(&bad_magic),
        Err(Error::InvalidMagic(12))
    ));

    let mut bad_width = header.encode(Endian::Little).unwrap();
    patch_byte(&mut bad_width, 6, 12);
    assert!(matches!(
        v1::Header::decode(&bad_width),
        Err(Error::InvalidAddressOffsetSize {
            version: 1,
            size: 12
        })
    ));

    let mut bad_uuid = header.encode(Endian::Little).unwrap();
    patch_byte(&mut bad_uuid, 7, 128);
    assert!(matches!(
        v1::Header::decode(&bad_uuid),
        Err(Error::InvalidUuidSize(128))
    ));
}

#[test]
fn v1_layout_matches_reference_tables_and_rejects_bad_string_ranges() {
    let header = v1::Header {
        address_offset_size: 2,
        base_address: 0,
        address_count: 11,
        string_table_offset: 136,
        string_table_size: 167,
        uuid: v1::FixedUuid::default(),
    };
    let layout = header.layout(400).unwrap();
    assert_eq!(layout.address_offsets, 48..70);
    assert_eq!(layout.address_info_offsets, 72..116);
    assert_eq!(layout.file_table_offset, 116);
    assert_eq!(layout.string_table, 136..303);
    assert_eq!(layout.function_info_offset, 303);

    let mut overlapping = header.clone();
    overlapping.string_table_offset = 100;
    assert!(overlapping.layout(400).is_err());
    let mut truncated = header;
    truncated.string_table_offset = 390;
    assert!(truncated.layout(400).is_err());
}

#[test]
fn v2_headers_accept_supported_widths_and_reject_invalid_fields() {
    for endian in ENDIANS {
        for width in 1..=8 {
            let header = v2::Header {
                address_offset_size: width,
                string_table_encoding: 0,
                base_address: 0x1000,
                address_count: 1,
            };
            let bytes = header.encode(endian).unwrap();
            assert_eq!(bytes.len(), v2::HEADER_SIZE);
            assert_eq!(v2::Header::decode(&bytes).unwrap(), (header, endian));
        }
    }

    let invalid_width = v2::Header {
        address_offset_size: 0,
        string_table_encoding: 0,
        base_address: 0,
        address_count: 1,
    };
    assert!(matches!(
        invalid_width.encode(Endian::Little),
        Err(Error::InvalidAddressOffsetSize {
            version: 2,
            size: 0
        })
    ));
    let invalid_encoding = v2::Header {
        address_offset_size: 4,
        string_table_encoding: 12,
        base_address: 0,
        address_count: 1,
    };
    assert!(matches!(
        invalid_encoding.encode(Endian::Little),
        Err(Error::UnsupportedStringTableEncoding(12))
    ));

    for length in 0..v2::HEADER_SIZE {
        assert!(v2::Header::decode([0; v2::HEADER_SIZE].get(..length).unwrap()).is_err());
    }

    let valid = v2::Header {
        address_offset_size: 4,
        string_table_encoding: 0,
        base_address: 0x1000,
        address_count: 1,
    };
    let mut bad_magic = valid.encode(Endian::Little).unwrap();
    patch_bytes(&mut bad_magic, 0, &12_u32.to_le_bytes());
    assert!(matches!(
        v2::Header::decode(&bad_magic),
        Err(Error::InvalidMagic(12))
    ));

    let mut bad_version = valid.encode(Endian::Little).unwrap();
    patch_bytes(&mut bad_version, 4, &12_u16.to_le_bytes());
    assert!(matches!(
        v2::Header::decode(&bad_version),
        Err(Error::UnsupportedVersion(12))
    ));

    let mut bad_width = valid.encode(Endian::Little).unwrap();
    patch_byte(&mut bad_width, 6, 12);
    assert!(matches!(
        v2::Header::decode(&bad_width),
        Err(Error::InvalidAddressOffsetSize {
            version: 2,
            size: 12
        })
    ));

    let mut bad_encoding = valid.encode(Endian::Little).unwrap();
    patch_byte(&mut bad_encoding, 7, 12);
    assert!(matches!(
        v2::Header::decode(&bad_encoding),
        Err(Error::UnsupportedStringTableEncoding(12))
    ));
}

fn encoded_directory(
    endian: Endian,
    header: &v2::Header,
    uuid_size: usize,
    file_count: u32,
    string_size: usize,
    function_size: usize,
) -> (v2::WriteLayout, Vec<u8>) {
    let layout = header
        .write_layout(uuid_size, file_count, string_size, function_size)
        .unwrap();
    let mut output = Encoder::new(endian);
    output.write_bytes(&header.encode(endian).unwrap());
    for entry in &layout.directory {
        entry.encode_into(&mut output);
    }
    v2::GlobalData {
        section_type: v2::GLOBAL_END,
        file_offset: 0,
        file_size: 0,
    }
    .encode_into(&mut output);
    let mut bytes = output.into_inner();
    bytes.resize(layout.file_size, 0);
    patch_byte(&mut bytes, layout.string_table.start, 0);
    (layout, bytes)
}

#[test]
fn v2_directory_layout_matches_reference_with_and_without_uuid() {
    for endian in ENDIANS {
        for uuid_size in [0, 13] {
            let header = v2::Header {
                address_offset_size: 8,
                string_table_encoding: 0,
                base_address: 0x1000,
                address_count: 3,
            };
            let (layout, bytes) = encoded_directory(endian, &header, uuid_size, 2, 17, 91);
            let directory = v2::parse_directory(&bytes, endian, &header).unwrap();
            assert_eq!(directory.end_offset, layout.directory_end);
            assert_eq!(directory.entries, layout.directory);
            assert_eq!(layout.address_offsets.start % 8, 0);
            assert_eq!(layout.address_info_offsets.start % 8, 0);
            assert_eq!(layout.file_table.start % 4, 0);
            assert_eq!(layout.function_info.start % 4, 0);
            assert_eq!(layout.uuid.is_some(), uuid_size != 0);
            for section in &layout.directory {
                assert!(section.file_offset + section.file_size <= bytes.len() as u64);
            }
        }
    }
}

#[test]
fn v2_write_layout_matches_reference_offsets() {
    let header = v2::Header {
        address_offset_size: 8,
        string_table_encoding: 0,
        base_address: 0,
        address_count: 3,
    };
    let layout = header.write_layout(20, 2, 17, 91).unwrap();
    assert_eq!(layout.directory_end, 160);
    assert_eq!(layout.uuid, Some(160..180));
    assert_eq!(layout.directory.len(), 6);
}

#[test]
fn v2_directory_rejects_reference_errors_and_structural_corruption() {
    let endian = Endian::Little;
    let header = v2::Header {
        address_offset_size: 1,
        string_table_encoding: 0,
        base_address: 0x1000,
        address_count: 1,
    };
    let (layout, bytes) = encoded_directory(endian, &header, 0, 1, 6, 16);

    for length in 0..layout.directory_end {
        assert!(v2::parse_directory(bytes.split_at(length).0, endian, &header).is_err());
    }

    let mut duplicate = Encoder::new(endian);
    duplicate.write_bytes(&header.encode(endian).unwrap());
    let first = layout.directory.first().unwrap();
    first.encode_into(&mut duplicate);
    first.encode_into(&mut duplicate);
    v2::GlobalData {
        section_type: v2::GLOBAL_END,
        file_offset: 0,
        file_size: 0,
    }
    .encode_into(&mut duplicate);
    let mut duplicate = duplicate.into_inner();
    duplicate.resize(bytes.len(), 0);
    assert!(matches!(
        v2::parse_directory(&duplicate, endian, &header),
        Err(Error::DuplicateSection(_))
    ));

    let mut missing = Encoder::new(endian);
    missing.write_bytes(&header.encode(endian).unwrap());
    for entry in layout.directory.split_at(layout.directory.len() - 1).0 {
        entry.encode_into(&mut missing);
    }
    v2::GlobalData {
        section_type: v2::GLOBAL_END,
        file_offset: 0,
        file_size: 0,
    }
    .encode_into(&mut missing);
    let mut missing = missing.into_inner();
    missing.resize(bytes.len(), 0);
    assert!(matches!(
        v2::parse_directory(&missing, endian, &header),
        Err(Error::MissingSection(v2::GLOBAL_FUNCTION_INFO))
    ));

    let mut zero_sized = bytes.clone();
    let first_size = v2::HEADER_SIZE + 12;
    patch_bytes(&mut zero_sized, first_size, &[0; 8]);
    assert!(matches!(
        v2::parse_directory(&zero_sized, endian, &header),
        Err(Error::ZeroSizedSection { .. })
    ));

    let mut outside = bytes.clone();
    patch_bytes(&mut outside, first_size, &u64::MAX.to_le_bytes());
    assert!(v2::parse_directory(&outside, endian, &header).is_err());

    let mut overlapping = bytes.clone();
    let function_entry = v2::HEADER_SIZE + 4 * v2::GLOBAL_DATA_SIZE;
    patch_bytes(
        &mut overlapping,
        function_entry + 4,
        &u64::try_from(layout.file_table.start)
            .unwrap()
            .to_le_bytes(),
    );
    assert!(v2::parse_directory(&overlapping, endian, &header).is_err());

    let mut bad_terminator = bytes;
    let terminator_offset = layout.directory_end - v2::GLOBAL_DATA_SIZE;
    patch_byte(&mut bad_terminator, terminator_offset + 4, 1);
    assert!(v2::parse_directory(&bad_terminator, endian, &header).is_err());
}

#[test]
fn header_magic_constant_matches_llvm() {
    assert_eq!(GSYM_MAGIC, 0x4753_594d);
    assert_eq!(v1::file_table_size(0).unwrap(), 4);
    assert_eq!(v1::file_table_size(3).unwrap(), 28);
    assert_eq!(v2::file_table_size(0).unwrap(), 4);
    assert_eq!(v2::file_table_size(3).unwrap(), 52);
}

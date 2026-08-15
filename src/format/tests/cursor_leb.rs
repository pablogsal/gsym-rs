use crate::endian::{Cursor, Encoder, Endian};
use crate::error::Error;
use crate::format::leb::{
    read_sleb, read_sleb_bounded, read_uleb, read_uleb_bounded, write_sleb, write_uleb,
};

use super::ENDIANS;

#[test]
fn data_extractor_reads_every_integer_width_and_endian() {
    let source = [1_u8, 2, 3, 4, 5, 6, 7, 8];
    let expected_little = [
        0x01,
        0x0201,
        0x0003_0201,
        0x0403_0201,
        0x0005_0403_0201,
        0x0605_0403_0201,
        0x0007_0605_0403_0201,
        0x0807_0605_0403_0201,
    ];
    let expected_big = [
        0x01,
        0x0102,
        0x0001_0203,
        0x0102_0304,
        0x0001_0203_0405,
        0x0102_0304_0506,
        0x0001_0203_0405_0607,
        0x0102_0304_0506_0708,
    ];

    for width in 1..=8 {
        let mut little = Cursor::new(&source, Endian::Little);
        let mut big = Cursor::new(&source, Endian::Big);
        assert_eq!(
            little.read_uint(width).unwrap(),
            expected_little
                .get(usize::from(width - 1))
                .copied()
                .unwrap()
        );
        assert_eq!(
            big.read_uint(width).unwrap(),
            expected_big.get(usize::from(width - 1)).copied().unwrap()
        );
        assert_eq!(little.position(), usize::from(width));
        assert_eq!(big.position(), usize::from(width));
    }

    assert!(Cursor::at(&source, Endian::Little, source.len() + 1).is_err());
    let mut short = Cursor::at(&source, Endian::Little, 7).unwrap();
    assert!(short.read_u16().is_err());
}

#[test]
fn file_writer_round_trips_fixed_width_leb_alignment_and_fixups() {
    for endian in ENDIANS {
        let mut output = Encoder::new(endian);
        output.write_u8(0x10);
        output.write_u16(0x1122);
        output.write_u32(0x1234_5678);
        output.write_u64(0x3344_5566_7788_99aa);
        output.align_to(16).unwrap();
        let fixup = output.len();
        output.write_u32(0);
        write_sleb(&mut output, i64::MIN);
        write_sleb(&mut output, i64::MAX);
        write_uleb(&mut output, 0);
        write_uleb(&mut output, u64::MAX);
        output.patch_u32(fixup, 0x1234_5678).unwrap();

        let mut input = Cursor::new(output.as_slice(), endian);
        assert_eq!(input.read_u8().unwrap(), 0x10);
        assert_eq!(input.read_u16().unwrap(), 0x1122);
        assert_eq!(input.read_u32().unwrap(), 0x1234_5678);
        assert_eq!(input.read_u64().unwrap(), 0x3344_5566_7788_99aa);
        input.take(16 - input.position() % 16).unwrap();
        assert_eq!(input.read_u32().unwrap(), 0x1234_5678);
        assert_eq!(read_sleb(&mut input).unwrap(), i64::MIN);
        assert_eq!(read_sleb(&mut input).unwrap(), i64::MAX);
        assert_eq!(read_uleb(&mut input).unwrap(), 0);
        assert_eq!(read_uleb(&mut input).unwrap(), u64::MAX);
        assert!(input.is_empty());
    }
}

#[test]
fn unsigned_writer_supports_widths_one_through_eight() {
    let values = [
        0x01,
        0x0102,
        0x0001_0203,
        0x0102_0304,
        0x0001_0203_0405,
        0x0102_0304_0506,
        0x0001_0203_0405_0607,
        0x0102_0304_0506_0708,
    ];
    for endian in ENDIANS {
        let mut output = Encoder::new(endian);
        for (index, value) in values.into_iter().enumerate() {
            output
                .write_uint(value, u8::try_from(index + 1).unwrap())
                .unwrap();
        }
        let mut input = Cursor::new(output.as_slice(), endian);
        for (index, value) in values.into_iter().enumerate() {
            assert_eq!(
                input.read_uint(u8::try_from(index + 1).unwrap()).unwrap(),
                value
            );
        }
        assert!(input.is_empty());
        assert!(Encoder::new(endian).write_uint(0x100, 1).is_err());
    }
}

#[test]
fn bounded_leb_rejects_truncation_overflow_and_bad_widths() {
    for endian in ENDIANS {
        let mut unterminated = Cursor::new(&[0x80; 10], endian);
        assert!(matches!(
            read_uleb(&mut unterminated),
            Err(Error::MalformedUleb { .. })
        ));

        let mut too_large = Cursor::new(&[0x80, 0x02], endian);
        assert!(read_uleb_bounded(&mut too_large, 8).is_err());

        let mut signed_too_large = Cursor::new(&[0x80, 0x01], endian);
        assert!(read_sleb_bounded(&mut signed_too_large, 8).is_err());

        let mut value = Cursor::new(&[0], endian);
        assert!(read_uleb_bounded(&mut value, 0).is_err());
        let mut value = Cursor::new(&[0], endian);
        assert!(read_sleb_bounded(&mut value, 65).is_err());
    }
}

#[test]
fn leb_examples_round_trip() {
    let unsigned = [0_u64, 1, 127, 128, 624_485, u64::MAX];
    for value in unsigned {
        let mut encoded = Encoder::new(Endian::Little);
        write_uleb(&mut encoded, value);
        let mut cursor = Cursor::new(encoded.as_slice(), Endian::Big);
        assert_eq!(read_uleb(&mut cursor).unwrap(), value);
        assert!(cursor.is_empty());
    }

    let signed = [i64::MIN, -624_485, -65, -64, -1, 0, 63, 64, i64::MAX];
    for value in signed {
        let mut encoded = Encoder::new(Endian::Little);
        write_sleb(&mut encoded, value);
        let mut cursor = Cursor::new(encoded.as_slice(), Endian::Big);
        assert_eq!(read_sleb(&mut cursor).unwrap(), value);
        assert!(cursor.is_empty());
    }
}

#[test]
fn thirty_two_bit_requests_accept_their_width_and_reject_the_next_value() {
    let mut encoded = Encoder::new(Endian::Little);
    write_uleb(&mut encoded, 0xffff_ffff);
    write_uleb(&mut encoded, 0x1_0000_0000);
    let mut cursor = Cursor::new(encoded.as_slice(), Endian::Little);
    assert_eq!(read_uleb_bounded(&mut cursor, 32).unwrap(), 0xffff_ffff);
    assert!(matches!(
        read_uleb_bounded(&mut cursor, 32),
        Err(Error::MalformedUleb {
            reason: "value exceeds requested bit width",
            ..
        })
    ));

    let mut encoded = Encoder::new(Endian::Little);
    write_sleb(&mut encoded, -2_147_483_648);
    write_sleb(&mut encoded, -2_147_483_649);
    let mut cursor = Cursor::new(encoded.as_slice(), Endian::Little);
    assert_eq!(read_sleb_bounded(&mut cursor, 32).unwrap(), -2_147_483_648);
    assert!(matches!(
        read_sleb_bounded(&mut cursor, 32),
        Err(Error::MalformedSleb {
            reason: "value exceeds requested bit width",
            ..
        })
    ));
}

#[test]
fn sixty_four_bit_requests_match_the_unbounded_readers() {
    let unsigned_overflow = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02];
    let mut plain = Cursor::new(&unsigned_overflow, Endian::Little);
    let mut bounded = Cursor::new(&unsigned_overflow, Endian::Little);
    let plain_error = read_uleb(&mut plain).unwrap_err().to_string();
    assert_eq!(
        plain_error,
        read_uleb_bounded(&mut bounded, 64).unwrap_err().to_string()
    );
    assert!(plain_error.contains("value exceeds u64"));

    let signed_overflow = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x40];
    let mut plain = Cursor::new(&signed_overflow, Endian::Little);
    let mut bounded = Cursor::new(&signed_overflow, Endian::Little);
    let plain_error = read_sleb(&mut plain).unwrap_err().to_string();
    assert_eq!(
        plain_error,
        read_sleb_bounded(&mut bounded, 64).unwrap_err().to_string()
    );
    assert!(plain_error.contains("value exceeds i64"));

    let mut encoded = Encoder::new(Endian::Little);
    write_uleb(&mut encoded, u64::MAX);
    let mut cursor = Cursor::new(encoded.as_slice(), Endian::Little);
    assert_eq!(read_uleb_bounded(&mut cursor, 64).unwrap(), u64::MAX);

    let mut encoded = Encoder::new(Endian::Little);
    write_sleb(&mut encoded, i64::MIN);
    let mut cursor = Cursor::new(encoded.as_slice(), Endian::Little);
    assert_eq!(read_sleb_bounded(&mut cursor, 64).unwrap(), i64::MIN);
}

#[test]
fn fixed_width_reads_and_patches_reject_out_of_bounds() {
    let mut input = Cursor::new(&[1, 2], Endian::Little);
    assert!(input.read_u32().is_err());

    let mut output = Encoder::new(Endian::Little);
    output.write_u8(1);
    output.align_to(4).unwrap();
    assert_eq!(output.as_slice(), &[1, 0, 0, 0]);
    assert!(output.patch_u32(1, 7).is_err());
}

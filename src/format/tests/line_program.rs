use crate::format::line;
use crate::model::LineEntry;

use super::ENDIANS;

#[test]
fn line_table_handles_mixed_delta_and_same_address_rows() {
    let base = 0x1000;
    let lines = vec![
        LineEntry {
            address: base,
            file: 1.into(),
            line: 10,
        },
        LineEntry {
            address: base + 0x10,
            file: 1.into(),
            line: 11,
        },
        LineEntry {
            address: base + 0x100,
            file: 1.into(),
            line: 1000,
        },
        LineEntry {
            address: base + 0x120,
            file: 1.into(),
            line: 900,
        },
        LineEntry {
            address: base + 0x120,
            file: 2.into(),
            line: 2000,
        },
        LineEntry {
            address: base + 0x121,
            file: 2.into(),
            line: 2001,
        },
        LineEntry {
            address: base + 0x122,
            file: 2.into(),
            line: 2002,
        },
        LineEntry {
            address: base + 0x123,
            file: 2.into(),
            line: 2003,
        },
    ];
    for endian in ENDIANS {
        let bytes = line::encode(&lines, endian, base).unwrap();
        assert_eq!(line::decode(&bytes, endian, base).unwrap(), lines);
        assert_eq!(line::lookup(&bytes, endian, base, base - 1).unwrap(), None);
        assert_eq!(
            line::lookup(&bytes, endian, base, base + 0x11).unwrap(),
            lines.get(1).copied()
        );
        assert_eq!(
            line::lookup(&bytes, endian, base, base + 0x200).unwrap(),
            lines.last().copied()
        );
    }
}

#[test]
fn line_table_reports_truncation_and_encode_errors() {
    let base = 0x1000;
    let lines = [
        LineEntry {
            address: base,
            file: 1.into(),
            line: 10,
        },
        LineEntry {
            address: base + 0x10,
            file: 1.into(),
            line: 11,
        },
    ];
    for endian in ENDIANS {
        let bytes = line::encode(&lines, endian, base).unwrap();
        for length in 0..bytes.len() {
            assert!(line::decode(bytes.get(..length).unwrap(), endian, base).is_err());
        }
        assert!(line::encode(&[], endian, base).is_err());
        assert!(line::encode(&lines, endian, base + 1).is_err());
        assert!(line::encode(&[lines[1], lines[0]], endian, base).is_err());

        let malformed_programs: [&[u8]; 5] = [
            &[1],
            &[1, 10],
            &[1, 10, 20],
            &[1, 10, 20, 1],
            &[1, 10, 20, 1, 5, 2],
        ];
        for bytes in malformed_programs {
            assert!(line::decode(bytes, endian, base).is_err());
        }
    }
}

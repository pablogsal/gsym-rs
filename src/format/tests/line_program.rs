use crate::endian::Endian;
use crate::format::line;
use crate::model::LineEntry;
use proptest::prelude::*;

use super::ENDIANS;

proptest! {
    #[test]
    fn delta_range_matches_the_most_frequent_window(
        numbers in prop::collection::vec(prop_oneof![0_u32..100, any::<u32>()], 1..200),
    ) {
        let lines: Vec<_> = numbers
            .iter()
            .enumerate()
            .map(|(index, &line)| LineEntry {
                address: index as u64,
                file: 1.into(),
                line,
            })
            .collect();
        let deltas: Vec<_> = numbers
            .windows(2)
            .filter_map(|pair| match pair {
                [previous, current] => {
                    Some(i64::from(*current).saturating_sub(i64::from(*previous)))
                }
                _ => None,
            })
            .collect();
        let mut distinct = deltas.clone();
        distinct.sort_unstable();
        distinct.dedup();
        let (mut low, mut high) = (0, 0);
        let mut best = 0;
        for &first in &distinct {
            let window: Vec<_> = deltas
                .iter()
                .copied()
                .filter(|delta| (first..=first.saturating_add(14)).contains(delta))
                .collect();
            if window.len() > best {
                best = window.len();
                low = first;
                high = window.into_iter().max().unwrap();
            }
        }
        if low == high && (1..14).contains(&low) {
            low = 0;
        }
        let bytes = line::encode(&lines, Endian::Little, 0).unwrap();
        let mut cursor = crate::endian::Cursor::new(&bytes, Endian::Little);
        prop_assert_eq!(crate::format::leb::read_sleb(&mut cursor).unwrap(), low);
        prop_assert_eq!(crate::format::leb::read_sleb(&mut cursor).unwrap(), high);
        prop_assert_eq!(line::decode(&bytes, Endian::Little, 0).unwrap(), lines);
    }
}

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

    let little = line::encode(&lines, Endian::Little, base).unwrap();
    assert_eq!(line::decode(&little, Endian::Big, base).unwrap(), lines);
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
            assert!(line::decode(bytes.split_at(length).0, endian, base).is_err());
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

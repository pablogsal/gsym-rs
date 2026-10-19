use super::*;
use crate::format::function::{EncodedFunction, EncodedInlineNode, encode};
use crate::model::AddressRange;

fn compare_scanners(payload: &[u8], endian: Endian, width: u8, address: u64) {
    let mut original = Cursor::new(payload, endian);
    let mut optimized = original.clone();
    let mut old_frames = SmallVec::new();
    let mut new_frames = SmallVec::new();
    let expected = scan_reference(
        &mut original,
        0x1000,
        true,
        0,
        &mut InlineScan {
            string_offset_size: width,
            address,
            frames: &mut old_frames,
        },
    )
    .map_err(|error| format!("{error:?}"));
    let actual = scan_inline_node(
        &mut optimized,
        0x1000,
        0,
        &mut InlineScan {
            string_offset_size: width,
            address,
            frames: &mut new_frames,
        },
    )
    .map_err(|error| format!("{error:?}"));
    assert_eq!(actual, expected, "payload: {payload:?}");
    assert_eq!(optimized.position(), original.position());
    assert_eq!(format!("{new_frames:?}"), format!("{old_frames:?}"));
}

fn fixture(depth: usize, endian: Endian, width: u8) -> Vec<u8> {
    let leaf = EncodedInlineNode {
        ranges: vec![AddressRange::new(0x1000, 0x1010)],
        name: 2,
        ..EncodedInlineNode::default()
    };
    let mut deep = leaf.clone();
    for _ in 0..depth {
        deep = EncodedInlineNode {
            children: vec![deep],
            ..leaf.clone()
        };
    }
    let range = AddressRange::new(0x1000, 0x1100);
    let function = EncodedFunction {
        range,
        name: 1,
        inline: Some(EncodedInlineNode {
            ranges: vec![range],
            name: 1,
            children: vec![leaf, deep],
            ..EncodedInlineNode::default()
        }),
        ..EncodedFunction::default()
    };
    let bytes = encode(&function, endian, width).unwrap();
    let mut cursor = Cursor::new(&bytes, endian);
    cursor.take(4 + usize::from(width)).unwrap();
    assert_eq!(cursor.read_u32().unwrap(), crate::format::INFO_INLINE);
    let length = usize::try_from(cursor.read_u32().unwrap()).unwrap();
    cursor.take(length).unwrap().to_vec()
}

#[test]
fn skipped_inline_lists_preserve_frames_errors_and_consumed_bytes() {
    for endian in [Endian::Little, Endian::Big] {
        for width in [4, 8] {
            for depth in [0, 1, 7, 8, 9, 16, 255] {
                let payload = fixture(depth, endian, width);
                for address in [0x1000, 0x1050] {
                    compare_fixture_mutations(&payload, endian, width, address);
                }
            }
        }
    }
}

fn compare_fixture_mutations(payload: &[u8], endian: Endian, width: u8, address: u64) {
    compare_scanners(payload, endian, width, address);
    for end in 0..payload.len() {
        compare_scanners(payload.split_at(end).0, endian, width, address);
    }
    for index in 0..payload.len() {
        let mut mutated = payload.to_vec();
        if let Some(byte) = mutated.get_mut(index) {
            *byte ^= 0xff;
        }
        compare_scanners(&mutated, endian, width, address);
    }
}

proptest::proptest! {
    #[test]
    fn inline_scanners_agree_on_arbitrary_payloads(
        bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256),
        address in proptest::prelude::any::<u64>(),
    ) {
        compare_scanners(&bytes, Endian::Little, 4, address);
    }
}

// Independent reference: the original recursive scanner, including its
// depth checks for list terminators and selective file/line validation.
fn scan_reference(
    cursor: &mut Cursor<'_>,
    base: u64,
    collect: bool,
    depth: usize,
    scan: &mut InlineScan<'_>,
) -> Result<(bool, bool)> {
    check_inline_depth(depth)?;
    let count = read_uleb(cursor)?;
    if count == 0 {
        return Ok((false, false));
    }
    if count > cursor.remaining() as u64 / 2 {
        return Err(Error::InvalidFormat(
            "inline range count exceeds remaining payload",
        ));
    }
    let mut contains = false;
    let mut first_start = None;
    for _ in 0..count {
        let start = base
            .checked_add(read_uleb(cursor)?)
            .ok_or(Error::Overflow("inline range start"))?;
        let end = start
            .checked_add(read_uleb(cursor)?)
            .ok_or(Error::Overflow("inline range end"))?;
        first_start.get_or_insert(start);
        if collect {
            contains |= start <= scan.address && scan.address < end;
        }
    }
    let first_start = first_start.ok_or(Error::InvalidFormat(
        "inline node declares no address range",
    ))?;
    let has_children = cursor.read_u8()? != 0;
    let name = cursor.read_uint(scan.string_offset_size)?;
    let call_file = read_uleb(cursor)?;
    let call_line = read_uleb(cursor)?;
    let matched = collect && contains;
    let original_len = scan.frames.len();
    if matched && name != 0 {
        scan.frames.push(RawInlineFrame {
            name,
            call_file: FileIndex::new(u32::try_from(call_file).map_err(|_| Error::OutOfRange {
                field: "inline call-file index",
                value: call_file,
                max: u64::from(u32::MAX),
            })?),
            call_line: u32::try_from(call_line).map_err(|_| Error::OutOfRange {
                field: "inline call-line",
                value: call_line,
                max: u64::from(u32::MAX),
            })?,
            start: first_start,
        });
    }
    if has_children {
        let child_base = first_start;
        let mut found_child = false;
        loop {
            let (present, child_matched) = scan_reference(
                cursor,
                child_base,
                matched && !found_child,
                depth.saturating_add(1),
                scan,
            )?;
            if !present {
                break;
            }
            found_child |= child_matched;
        }
    }
    if !matched {
        scan.frames.truncate(original_len);
    }
    Ok((true, matched))
}

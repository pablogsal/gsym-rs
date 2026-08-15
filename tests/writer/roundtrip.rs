//! Building an image, reading it back, and comparing the two sides.

use gsym::{
    AddressRange, Endian, FileEntry, FrameLookupOptions, Function, Gsym, GsymBuilder, GsymVersion,
    InlineNode, LineEntry, LookupScratch,
};
use proptest::prelude::*;

pub(crate) fn builder_with_lines(version: GsymVersion, endian: Endian) -> GsymBuilder {
    let mut builder = GsymBuilder::new()
        .version(version)
        .endian(endian)
        .base_address(0x0040_0000)
        .build_id(vec![0x12, 0x34, 0x56, 0x78]);
    let file = builder
        .add_file(FileEntry::new(
            b"/build/project/src".to_vec(),
            b"main.rs".to_vec(),
        ))
        .unwrap();
    builder
        .add_function(Function {
            lines: vec![
                LineEntry {
                    address: 0x0040_1000,
                    file,
                    line: 10,
                },
                LineEntry {
                    address: 0x0040_1010,
                    file,
                    line: 11,
                },
            ],
            ..Function::new(
                AddressRange::new(0x0040_1000, 0x0040_1020),
                b"first".to_vec(),
            )
        })
        .unwrap();
    builder
        .add_function(Function {
            lines: vec![LineEntry {
                address: 0x0040_2000,
                file,
                line: 99,
            }],
            ..Function::new(
                AddressRange::new(0x0040_2000, 0x0040_2008),
                b"second".to_vec(),
            )
        })
        .unwrap();
    builder
}

#[test]
fn all_versions_and_byte_orders_roundtrip() {
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            let bytes = builder_with_lines(version, endian).to_bytes().unwrap();
            let gsym = Gsym::parse(&bytes).unwrap();
            gsym.verify().unwrap();
            assert_eq!(gsym.build_id(), &[0x12, 0x34, 0x56, 0x78]);

            let first = gsym.lookup(0x0040_1000).unwrap().unwrap();
            assert_eq!(first.frames[0].name, b"first");
            assert_eq!(first.frames[0].basename, b"main.rs");
            assert_eq!(first.frames[0].line, 10);

            let next_line = gsym.lookup(0x0040_1010).unwrap().unwrap();
            assert_eq!(next_line.frames[0].line, 11);

            let second = gsym.lookup(0x0040_2007).unwrap().unwrap();
            assert_eq!(second.frames[0].name, b"second");
            assert_eq!(second.frames[0].line, 99);
        }
    }
}

#[test]
fn inline_frames_are_innermost_first_and_support_a_visitor() {
    let mut builder = GsymBuilder::new();
    let file = builder
        .add_file(FileEntry::new(b"/src".to_vec(), b"inline.rs".to_vec()))
        .unwrap();
    builder
        .add_function(Function {
            lines: vec![LineEntry {
                address: 0x1020,
                file,
                line: 30,
            }],
            inline: Some(InlineNode {
                ranges: vec![AddressRange::new(0x1000, 0x1100)],
                name: b"outer".to_vec(),
                call_file: 0.into(),
                call_line: 0,
                children: vec![InlineNode {
                    ranges: vec![AddressRange::new(0x1010, 0x1080)],
                    name: b"inline_a".to_vec(),
                    call_file: file,
                    call_line: 10,
                    children: vec![InlineNode {
                        ranges: vec![AddressRange::new(0x1020, 0x1040)],
                        name: b"inline_b".to_vec(),
                        call_file: file,
                        call_line: 20,
                        children: Vec::new(),
                    }],
                }],
            }),
            ..Function::new(AddressRange::new(0x1000, 0x1100), b"outer".to_vec())
        })
        .unwrap();

    let bytes = builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&bytes).unwrap();
    let lookup = gsym.lookup(0x1028).unwrap().unwrap();
    assert_eq!(
        lookup
            .frames
            .iter()
            .map(|frame| (frame.name, frame.line, frame.inlined))
            .collect::<Vec<_>>(),
        vec![
            (&b"inline_b"[..], 30, true),
            (&b"inline_a"[..], 20, true),
            (&b"outer"[..], 10, false),
        ]
    );

    let mut scratch = LookupScratch::with_capacity(3);
    let mut visited = Vec::new();
    assert!(
        gsym.for_each_frame(
            0x1028,
            FrameLookupOptions::default(),
            &mut scratch,
            |frame| {
                visited.push((frame.name, frame.line, frame.inlined));
            },
        )
        .unwrap()
    );
    assert_eq!(
        visited,
        lookup
            .frames
            .iter()
            .map(|frame| (frame.name, frame.line, frame.inlined))
            .collect::<Vec<_>>()
    );
}

proptest! {
    #[test]
    fn function_only_images_roundtrip(
        widths in proptest::collection::vec(1_u16..256, 1..128),
        use_v2 in any::<bool>(),
        use_big_endian in any::<bool>(),
    ) {
        let version = if use_v2 { GsymVersion::V2 } else { GsymVersion::V1 };
        let endian = if use_big_endian { Endian::Big } else { Endian::Little };
        let mut builder = GsymBuilder::new()
            .version(version)
            .endian(endian)
            .base_address(0x1000);
        for (index, width) in widths.iter().copied().enumerate() {
            let start = 0x1000 + (index as u64) * 0x100;
            builder.add_function(Function::new(AddressRange::new(start, start + u64::from(width)), format!("function_{index}").into_bytes())).unwrap();
        }

        let bytes = builder.to_bytes().unwrap();
        let gsym = Gsym::parse(&bytes).unwrap();
        gsym.verify().unwrap();
        prop_assert_eq!(gsym.functions().count(), widths.len());
        for index in 0..widths.len() {
            let address = 0x1000 + (index as u64) * 0x100;
            let lookup = gsym.lookup(address).unwrap().unwrap();
            let expected = format!("function_{index}");
            prop_assert_eq!(lookup.frames[0].name, expected.as_bytes());
        }
    }
}

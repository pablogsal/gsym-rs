//! Decoding a whole image and re-encoding it, as one file or as segments.

use gsym::{
    AddressRange, Endian, FileEntry, Function, Gsym, GsymBuilder, GsymVersion, InlineNode,
    LineEntry, TranscodeOptions,
};

use crate::bytes::ByteOrder;
use crate::minimal::{minimal_v1, minimal_v2};

fn source_image() -> Vec<u8> {
    let mut builder = GsymBuilder::new()
        .version(GsymVersion::V1)
        .endian(Endian::Little)
        .base_address(0x1000)
        .build_id(vec![1, 2, 3, 4]);
    let unused = builder
        .add_file(FileEntry::new(b"/unused".to_vec(), b"unused.c".to_vec()))
        .unwrap();
    let source = builder
        .add_file(FileEntry::new(b"/src".to_vec(), b"main.c".to_vec()))
        .unwrap();
    assert_eq!(unused.get(), 1);
    assert_eq!(source.get(), 2);
    for index in 0..12_u64 {
        let start = 0x1000 + index * 0x20;
        builder
            .add_function(Function {
                lines: vec![LineEntry::new(
                    start,
                    source,
                    u32::try_from(index + 1).unwrap(),
                )],
                inline: (index == 0).then(|| InlineNode {
                    ranges: vec![AddressRange::new(start + 1, start + 4)],
                    name: b"inlined".to_vec(),
                    call_file: source,
                    call_line: 7,
                    children: Vec::new(),
                }),
                ..Function::new(
                    AddressRange::new(start, start + 0x20),
                    format!("function_{index}").into_bytes(),
                )
            })
            .unwrap();
    }
    builder.to_bytes().unwrap()
}

#[test]
fn whole_file_transcoding_preserves_semantics_across_versions_and_endians() {
    let source = source_image();
    let parsed = Gsym::parse(&source).unwrap();
    let decoded = parsed.decode_all().unwrap();
    assert_eq!(decoded.files.len(), 3);
    assert_eq!(decoded.functions.len(), 12);

    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            let bytes = parsed
                .transcode(TranscodeOptions {
                    version: Some(version),
                    endian: Some(endian),
                })
                .unwrap();
            let output = Gsym::parse(&bytes).unwrap();
            output.verify().unwrap();
            assert_eq!(output.header().version, version);
            assert_eq!(output.header().endian, endian);
            assert_eq!(output.build_id(), &[1, 2, 3, 4]);
            let hit = output.lookup(0x1001).unwrap().unwrap();
            assert_eq!(hit.frames()[0].name, b"inlined");
            assert_eq!(hit.frames()[0].basename, b"main.c");
        }
    }
}

#[test]
fn segments_are_independent_size_bounded_and_cover_every_function() {
    let decoded = Gsym::parse(&source_image()).unwrap().decode_all().unwrap();
    let target = 260;
    let segments = decoded
        .segments(
            target,
            TranscodeOptions {
                version: Some(GsymVersion::V2),
                endian: Some(Endian::Big),
            },
        )
        .unwrap();
    assert!(segments.len() > 1);
    assert_eq!(
        segments
            .iter()
            .map(|segment| segment.function_count)
            .sum::<usize>(),
        decoded.functions.len()
    );
    for segment in &segments {
        let parsed = Gsym::parse(segment.bytes()).unwrap();
        let report = parsed.verify().unwrap();
        assert_eq!(report.functions, segment.function_count);
        assert_eq!(report.files, 2, "unused source files must not enter shards");
        if segment.function_count > 1 {
            assert!(segment.bytes().len() <= target);
        }
        assert!(parsed.lookup(segment.first_address).unwrap().is_some());
    }
}

#[test]
fn zero_segment_size_is_rejected() {
    let decoded = Gsym::parse(&source_image()).unwrap().decode_all().unwrap();
    assert!(decoded.segments(0, TranscodeOptions::default()).is_err());
}

#[test]
fn v2_to_v1_transcoding_reports_v1_identity_limits() {
    let mut builder = GsymBuilder::new()
        .version(GsymVersion::V2)
        .build_id(vec![0x5a; 21]);
    builder
        .add_function(Function::new(
            AddressRange::new(0x1000, 0x1010),
            b"identity".to_vec(),
        ))
        .unwrap();
    let source = builder.to_bytes().unwrap();
    let error = Gsym::parse(&source)
        .unwrap()
        .transcode(TranscodeOptions {
            version: Some(GsymVersion::V1),
            endian: None,
        })
        .unwrap_err();
    assert!(matches!(error, gsym::Error::V1BuildIdTooLong { size: 21 }));
}

#[test]
fn function_only_files_without_a_physical_file_zero_are_normalized() {
    for source in [minimal_v1(ByteOrder::Little), minimal_v2(ByteOrder::Big)] {
        let transcoded = Gsym::parse(&source)
            .unwrap()
            .transcode(TranscodeOptions::default())
            .unwrap();
        let parsed = Gsym::parse(&transcoded).unwrap();
        assert_eq!(parsed.verify().unwrap().files, 1);
        assert_eq!(parsed.file(0).unwrap(), (&[][..], &[][..]));
    }
}

#[test]
fn decoded_model_moves_into_builder_without_changing_semantics() {
    let decoded = Gsym::parse(source_image()).unwrap().decode_all().unwrap();
    let encoded = decoded
        .into_builder(TranscodeOptions {
            version: Some(GsymVersion::V2),
            endian: Some(Endian::Big),
        })
        .unwrap()
        .to_bytes()
        .unwrap();
    let parsed = Gsym::parse(encoded).unwrap();
    assert_eq!(parsed.header().version, GsymVersion::V2);
    assert_eq!(parsed.header().endian, Endian::Big);
    assert_eq!(
        parsed.lookup(0x1001).unwrap().unwrap().frames()[0].name,
        b"inlined"
    );
}

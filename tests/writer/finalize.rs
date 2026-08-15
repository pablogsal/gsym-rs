//! Finalization: the repairs and de-duplication applied on the way to bytes.

use gsym::{
    AddressRange, BuilderOptions, CallSite, CallSiteFlags, Endian, Error, FileEntry, Function,
    Gsym, GsymBuilder, GsymVersion, LineEntry, WriterOptions,
};

fn function(start: u64, size: u64, name: &[u8]) -> Function {
    Function::new(AddressRange::new(start, start + size), name.to_vec())
}

fn encoded(
    version: GsymVersion,
    endian: Endian,
    base: u64,
    functions: impl IntoIterator<Item = Function>,
) -> Vec<u8> {
    let mut builder = GsymBuilder::new()
        .version(version)
        .endian(endian)
        .base_address(base);
    for function in functions {
        builder.add_function(function).unwrap();
    }
    builder.to_bytes().unwrap()
}

#[test]
fn creator_selects_canonical_address_width_at_every_boundary() {
    let base = 0x1000;
    let cases = [
        (0, 1),
        (0x20, 1),
        (u64::from(u8::MAX), 1),
        (u64::from(u8::MAX) + 1, 2),
        (u64::from(u16::MAX), 2),
        (1_u64 << 16, 4),
        (1_u64 << 24, 4),
        (u64::from(u32::MAX), 4),
        (u64::from(u32::MAX) + 1, 8),
        (1_u64 << 40, 8),
        (1_u64 << 48, 8),
        (1_u64 << 56, 8),
    ];

    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            for (maximum_offset, expected_width) in cases {
                let mut functions = vec![function(base, 1, b"first")];
                if maximum_offset != 0 {
                    functions.push(function(base + maximum_offset, 1, b"last"));
                }
                let bytes = encoded(version, endian, base, functions);
                let gsym = Gsym::parse(&bytes).unwrap();

                assert_eq!(gsym.header().version, version);
                assert_eq!(gsym.header().endian, endian);
                assert_eq!(
                    gsym.header().address_offset_size,
                    expected_width,
                    "version={version:?}, endian={endian:?}, offset={maximum_offset:#x}"
                );
                gsym.verify().unwrap();
            }
        }
    }
}

#[test]
fn string_and_file_tables_reserve_zero_and_deduplicate_entries() {
    let mut builder = GsymBuilder::new();
    assert_eq!(builder.files(), &[FileEntry::default()]);
    assert_eq!(builder.add_file(FileEntry::default()).unwrap().get(), 0);

    let source = FileEntry::new(b"/workspace/src".to_vec(), b"main.rs".to_vec());
    let file = builder.add_file(source.clone()).unwrap();
    assert_eq!(file.get(), 1);
    assert_eq!(builder.add_file(source.clone()).unwrap(), file);

    builder
        .add_function(Function {
            lines: vec![LineEntry {
                address: 0x1000,
                file,
                line: 17,
            }],
            ..Function::new(AddressRange::new(0x1000, 0x1010), b"main".to_vec())
        })
        .unwrap();

    let bytes = builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&bytes).unwrap();
    assert_eq!(gsym.file(0).unwrap(), (&[][..], &[][..]));
    assert_eq!(
        gsym.file(1).unwrap(),
        (source.directory.as_slice(), source.basename.as_slice())
    );
    let found = gsym.lookup(0x1000).unwrap().unwrap();
    assert_eq!(found.frames[0].name, b"main");
    assert_eq!(found.frames[0].directory, b"/workspace/src");
    assert_eq!(found.frames[0].basename, b"main.rs");
}

#[test]
fn writer_compacts_repeated_source_locations() {
    let mut builder = GsymBuilder::new();
    let file = builder
        .add_file(FileEntry::new(b"/workspace/src", b"main.rs"))
        .unwrap();
    let other_file = builder
        .add_file(FileEntry::new(b"/workspace/src", b"other.rs"))
        .unwrap();
    builder
        .add_function(Function {
            lines: vec![
                LineEntry::new(0x1000, file, 10),
                LineEntry::new(0x1004, file, 10),
                LineEntry::new(0x1004, file, 11),
                LineEntry::new(0x1004, file, 10),
                LineEntry::new(0x1008, file, 10),
                LineEntry::new(0x1008, other_file, 10),
                LineEntry::new(0x100c, other_file, 10),
            ],
            merged: vec![Function {
                lines: vec![
                    LineEntry::new(0x1000, file, 20),
                    LineEntry::new(0x1004, file, 20),
                ],
                ..Function::new(AddressRange::new(0x1000, 0x1010), b"alias")
            }],
            ..Function::new(AddressRange::new(0x1000, 0x1010), b"main")
        })
        .unwrap();

    let bytes = builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&bytes).unwrap();
    let decoded = gsym.function(0).unwrap().decode().unwrap();

    assert_eq!(
        decoded
            .lines
            .iter()
            .map(|row| (row.address, row.file.get(), row.line))
            .collect::<Vec<_>>(),
        [
            (0x1000, file.get(), 10),
            (0x1004, file.get(), 11),
            (0x1004, file.get(), 10),
            (0x1008, other_file.get(), 10),
        ]
    );
    let alias = decoded.merged.first().unwrap();
    assert_eq!(alias.lines, [LineEntry::new(0x1000, file, 20)]);
    let lookup = gsym.lookup(0x1008).unwrap().unwrap();
    assert_eq!(lookup.frames[0].line, 10);
    assert_eq!(lookup.frames[0].basename, b"other.rs");
}

#[test]
fn finalize_repairs_the_last_zero_sized_symbol_to_the_text_end() {
    let options = BuilderOptions {
        executable_ranges: vec![AddressRange::new(0x1000, 0x1100)].into_boxed_slice(),
        ..BuilderOptions::default()
    };
    let mut builder = GsymBuilder::with_options(options);
    builder
        .add_function(function(0x1080, 0, b"last_symbol"))
        .unwrap();

    let bytes = builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&bytes).unwrap();
    let decoded = gsym.function(0).unwrap().decode().unwrap();
    assert_eq!(decoded.range, AddressRange::new(0x1080, 0x1100));
    assert_eq!(
        gsym.lookup(0x10ff).unwrap().unwrap().frames[0].name,
        b"last_symbol"
    );
}

#[test]
fn zero_sized_repair_updates_merged_alias_ranges() -> gsym::Result<()> {
    let mut builder = GsymBuilder::new()
        .merge_equal_address_functions(true)
        .executable_ranges([AddressRange::new(0x1000, 0x1100)]);
    builder.add_function(Function::new(AddressRange::new(0x1010, 0x1010), b"first"))?;
    builder.add_function(Function::new(AddressRange::new(0x1010, 0x1010), b"alias"))?;

    let bytes = builder.to_bytes()?;
    let gsym = Gsym::parse(bytes)?;
    gsym.verify()?;
    let function = gsym.function(0)?.decode()?;
    assert_eq!(function.range, AddressRange::new(0x1010, 0x1100));
    assert_eq!(function.merged.len(), 1);
    assert_eq!(function.merged[0].range, function.range);
    Ok(())
}

#[test]
fn zero_sized_repair_can_be_disabled() {
    let options = BuilderOptions {
        executable_ranges: vec![AddressRange::new(0x1000, 0x1100)].into_boxed_slice(),
        repair_zero_sized_functions: false,
        ..BuilderOptions::default()
    };
    let mut builder = GsymBuilder::with_options(options);
    builder
        .add_function(function(0x1080, 0, b"last_symbol"))
        .unwrap();

    let bytes = builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&bytes).unwrap();
    assert_eq!(
        gsym.function(0).unwrap().range(),
        AddressRange::new(0x1080, 0x1080)
    );
}

#[test]
fn finalize_combines_multiple_zero_sized_symbols_at_one_address() {
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            let bytes = encoded(
                version,
                endian,
                0x1800,
                [function(0x1800, 0, b"foo"), function(0x1800, 0, b"bar")],
            );
            let gsym = Gsym::parse(&bytes).unwrap();
            assert_eq!(gsym.functions().count(), 1);
            assert_eq!(
                gsym.function(0).unwrap().range(),
                AddressRange::new(0x1800, 0x1800)
            );
        }
    }
}

#[test]
fn finalize_keeps_rich_duplicate_information_and_the_itanium_name() {
    let rich = Function {
        lines: vec![LineEntry::new(0x2000, 0.into(), 42)],
        ..Function::new(AddressRange::new(0x2000, 0x2010), b"make_ftype".to_vec())
    };
    let plain = function(0x2000, 0x10, b"_Z10make_ftypePci");

    for version in [GsymVersion::V1, GsymVersion::V2] {
        for functions in [
            vec![plain.clone(), rich.clone()],
            vec![rich.clone(), plain.clone()],
        ] {
            let bytes = encoded(version, Endian::Little, 0x2000, functions);
            let gsym = Gsym::parse(&bytes).unwrap();
            assert_eq!(gsym.functions().count(), 1);
            let decoded = gsym.function(0).unwrap().decode().unwrap();
            assert_eq!(decoded.name, b"_Z10make_ftypePci");
            assert_eq!(decoded.lines, rich.lines);
        }
    }
}

#[test]
fn finalize_keeps_rich_duplicate_information_and_the_swift_name() {
    let rich = Function {
        lines: vec![LineEntry::new(0x3000, 0.into(), 9)],
        ..Function::new(AddressRange::new(0x3000, 0x3010), b"make_ftype".to_vec())
    };
    let plain = function(0x3000, 0x10, b"$s4main10make_ftypeyyF");
    let bytes = encoded(GsymVersion::V2, Endian::Big, 0x3000, [rich, plain]);
    let decoded = Gsym::parse(&bytes)
        .unwrap()
        .function(0)
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(decoded.name, b"$s4main10make_ftypeyyF");
    assert_eq!(decoded.lines.len(), 1);
}

#[test]
fn mangled_name_replacement_rejects_unrelated_names() {
    let rich = Function {
        lines: vec![LineEntry::new(0x4000, 0.into(), 5)],
        ..Function::new(AddressRange::new(0x4000, 0x4010), b"bar".to_vec())
    };
    let plain = function(0x4000, 0x10, b"_Z3foov");
    let bytes = encoded(GsymVersion::V1, Endian::Little, 0x4000, [plain, rich]);
    let decoded = Gsym::parse(&bytes)
        .unwrap()
        .function(0)
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(decoded.name, b"bar");
}

#[test]
fn duplicate_range_keeps_call_site_information() {
    let rich = Function {
        call_sites: vec![CallSite {
            return_offset: 4,
            flags: CallSiteFlags::INTERNAL,
            match_regex: vec![b"target.*".to_vec()],
        }],
        ..Function::new(AddressRange::new(0x5000, 0x5020), b"callee".to_vec())
    };
    let plain = function(0x5000, 0x20, b"callee");
    let bytes = encoded(GsymVersion::V2, Endian::Little, 0x5000, [rich, plain]);
    let gsym = Gsym::parse(&bytes).unwrap();
    assert_eq!(gsym.functions().count(), 1);
    let decoded = gsym.function(0).unwrap().decode().unwrap();
    assert_eq!(decoded.call_sites.len(), 1);
    assert_eq!(
        decoded.call_sites[0].match_regex,
        vec![b"target.*".to_vec()]
    );
    assert_eq!(
        gsym.lookup(0x5004).unwrap().unwrap().call_site_patterns,
        Box::from([&b"target.*"[..]])
    );
}

#[test]
fn merged_function_mode_preserves_equal_range_aliases() {
    let options = BuilderOptions {
        writer: WriterOptions {
            version: GsymVersion::V2,
            endian: Endian::Big,
            ..WriterOptions::default()
        },
        merge_equal_address_functions: true,
        ..BuilderOptions::default()
    };
    let mut builder = GsymBuilder::with_options(options);
    builder
        .add_function(function(0x6000, 0x20, b"alpha"))
        .unwrap();
    builder
        .add_function(Function {
            lines: vec![LineEntry::new(0x6000, 0.into(), 77)],
            ..Function::new(AddressRange::new(0x6000, 0x6020), b"zulu".to_vec())
        })
        .unwrap();

    let bytes = builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&bytes).unwrap();
    assert_eq!(gsym.functions().count(), 1);
    let decoded = gsym.function(0).unwrap().decode().unwrap();
    assert_eq!(decoded.name, b"alpha");
    assert_eq!(decoded.merged.len(), 1);
    assert_eq!(decoded.merged[0].name, b"zulu");
    assert_eq!(decoded.merged[0].lines.len(), 1);
}

#[test]
fn v1_rejects_oversized_build_id_while_v2_roundtrips_it() {
    let build_id = vec![0xa5; 21];
    let mut v1 = GsymBuilder::new()
        .version(GsymVersion::V1)
        .build_id(build_id.clone());
    v1.add_function(function(0x7000, 1, b"symbol")).unwrap();
    assert!(matches!(
        v1.to_bytes(),
        Err(Error::V1BuildIdTooLong { size: 21 })
    ));

    let mut v2 = GsymBuilder::new()
        .version(GsymVersion::V2)
        .build_id(build_id.clone());
    v2.add_function(function(0x7000, 1, b"symbol")).unwrap();
    let bytes = v2.to_bytes().unwrap();
    assert_eq!(Gsym::parse(&bytes).unwrap().build_id(), build_id);
}

#[test]
fn writer_is_deterministic() {
    let first = crate::roundtrip::builder_with_lines(GsymVersion::V1, Endian::Little)
        .to_bytes()
        .unwrap();
    let second = crate::roundtrip::builder_with_lines(GsymVersion::V1, Endian::Little)
        .to_bytes()
        .unwrap();
    assert_eq!(first, second);
}

#[test]
fn writer_output_is_deterministic_across_insertion_order() {
    let functions = [
        function(0x9000, 0x10, b"alpha"),
        function(0x9100, 0x20, b"beta"),
        function(0x9200, 0x30, b"gamma"),
    ];
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            let forward = encoded(version, endian, 0x9000, functions.clone());
            let reverse = encoded(version, endian, 0x9000, functions.clone().into_iter().rev());
            assert_eq!(forward, reverse, "version={version:?}, endian={endian:?}");
        }
    }
}

#[test]
fn duplicate_selection_counts_the_complete_inline_tree() {
    let range = AddressRange::new(0xa000, 0xa020);
    let shallow = Function {
        range,
        name: b"same".to_vec(),
        inline: Some(gsym::InlineNode {
            ranges: vec![range],
            name: b"same".to_vec(),
            ..gsym::InlineNode::default()
        }),
        ..Function::default()
    };
    let deep = Function {
        range,
        name: b"same".to_vec(),
        inline: Some(gsym::InlineNode {
            ranges: vec![range],
            name: b"same".to_vec(),
            children: vec![gsym::InlineNode {
                ranges: vec![AddressRange::new(0xa001, 0xa010)],
                name: b"nested".to_vec(),
                ..gsym::InlineNode::default()
            }],
            ..gsym::InlineNode::default()
        }),
        ..Function::default()
    };
    for functions in [vec![shallow.clone(), deep.clone()], vec![deep, shallow]] {
        let bytes = encoded(GsymVersion::V2, Endian::Little, range.start, functions);
        let decoded = Gsym::parse(&bytes)
            .unwrap()
            .function(0)
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(decoded.inline.unwrap().children.len(), 1);
    }
}

//! Builder configuration and the input it refuses to accept.

use gsym::{
    AddressRange, Endian, Error, FileEntry, Function, GsymBuilder, GsymVersion, InlineNode,
    LineEntry,
};

#[test]
fn file_interning_is_stable_and_reserves_zero() {
    let mut builder = GsymBuilder::new();
    let file = FileEntry::new(b"/workspace/src".to_vec(), b"main.rs".to_vec());

    let first = builder.add_file(file.clone()).unwrap();
    let duplicate = builder.add_file(file).unwrap();

    assert_eq!(first.get(), 1);
    assert_eq!(duplicate, first);
    assert_eq!(builder.files().len(), 2);
}

#[test]
fn builder_configuration_uses_value_semantics() {
    let ranges = [AddressRange::new(0x1000, 0x2000)];
    let builder = GsymBuilder::new()
        .version(GsymVersion::V2)
        .endian(Endian::Big)
        .base_address(0x1000)
        .build_id([1, 2, 3])
        .repair_zero_sized_functions(false)
        .merge_equal_address_functions(true)
        .executable_ranges(ranges);
    let options = builder.options();

    assert_eq!(options.writer.version, GsymVersion::V2);
    assert_eq!(options.writer.endian, Endian::Big);
    assert_eq!(options.writer.base_address, Some(0x1000));
    assert_eq!(options.writer.build_id, [1, 2, 3]);
    assert!(!options.repair_zero_sized_functions);
    assert!(options.merge_equal_address_functions);
    assert_eq!(options.executable_ranges.as_ref(), ranges);
}

#[test]
fn builder_rejects_an_empty_function_name() {
    let mut builder = GsymBuilder::new();
    // Deliberately a literal: `Function::new` cannot express a missing name.
    let function = Function {
        range: AddressRange::new(0x1000, 0x1010),
        ..Function::default()
    };

    assert!(matches!(
        builder.add_function(function),
        Err(Error::InvalidModel("function name must not be empty"))
    ));
}

#[test]
fn builder_rejects_reversed_ranges() {
    let mut builder = GsymBuilder::new();
    let function = Function::new(AddressRange::new(0x1010, 0x1000), b"reversed");

    assert!(matches!(
        builder.add_function(function),
        Err(Error::InvalidModel("function range end precedes its start"))
    ));
}

#[test]
fn builder_rejects_lines_outside_the_function() {
    let mut builder = GsymBuilder::new();
    let function = Function {
        lines: vec![LineEntry::new(0x1010, 0.into(), 7)],
        ..Function::new(AddressRange::new(0x1000, 0x1010), b"outside".to_vec())
    };

    assert!(matches!(
        builder.add_function(function),
        Err(Error::InvalidModel("line address is outside its function"))
    ));
}

#[test]
fn builder_rejects_invalid_inline_range_sets() {
    for (ranges, message) in [
        (
            vec![AddressRange::new(0x1008, 0x1004)],
            "inline range is empty, reversed, or outside its parent",
        ),
        (
            vec![AddressRange::new(0x1004, 0x1004)],
            "inline range is empty, reversed, or outside its parent",
        ),
        (
            vec![
                AddressRange::new(0x1008, 0x100c),
                AddressRange::new(0x1004, 0x1006),
            ],
            "inline ranges overlap or are not sorted",
        ),
        (
            vec![
                AddressRange::new(0x1004, 0x100c),
                AddressRange::new(0x1008, 0x100e),
            ],
            "inline ranges overlap or are not sorted",
        ),
    ] {
        let mut builder = GsymBuilder::new();
        assert!(matches!(
            builder.add_function(Function {
                inline: Some(InlineNode {
                    ranges,
                    ..InlineNode::default()
                }),
                ..Function::new(AddressRange::new(0x1000, 0x1010), b"invalid-inline")
            }),
            Err(Error::InvalidModel(reason)) if reason == message
        ));
    }
}

#[test]
fn model_rejects_function_sizes_that_do_not_fit_the_wire_format() {
    let mut builder = GsymBuilder::new();
    let error = builder
        .add_function(Function::new(
            AddressRange::new(0x8000, 0x8000 + u64::from(u32::MAX) + 1),
            b"too_large",
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Limit {
            context: "function size",
            limit,
            ..
        } if limit == u64::from(u32::MAX)
    ));
}

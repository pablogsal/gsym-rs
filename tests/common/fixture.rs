//! A hand-written GSYM encoder rich enough for line, inline and call-site data.
//!
//! Like `crate::minimal`, this encoder is deliberately independent of
//! `GsymBuilder` and the crate writer: it re-derives the on-disk layout from the
//! format documentation so that reader tests cannot be fooled by a bug the
//! writer shares. It emits only the subset of the format those tests need.

use gsym::GsymVersion;

use crate::bytes::{ByteOrder, align, as_u64, patch_uint, write_offset, write_uint};
use crate::leb::{sleb, uleb};

const MAGIC: u32 = 0x4753_594d;
const ADDRESS_WIDTH: u8 = 2;
const V1_HEADER_SIZE: usize = 48;
const V2_DIRECTORY_ENTRIES: usize = 6;
const V2_DIRECTORY_ENTRY_SIZE: usize = 20;

/// A function record before it is laid out in a file.
#[derive(Clone, Debug)]
pub(crate) struct RawFunction {
    pub address: u64,
    pub size: u32,
    pub name: u64,
    /// Optional TLV records, as `(type, payload)` pairs.
    pub records: Vec<(u32, Vec<u8>)>,
}

/// An encoded image plus the offsets tests need in order to corrupt it.
#[derive(Debug)]
pub(crate) struct Fixture {
    pub bytes: Vec<u8>,
    /// Where each function's offset is recorded in the address-info table.
    pub address_info_slots: Vec<usize>,
    /// Where each function record stores its name's string-table offset.
    pub name_slots: Vec<usize>,
    pub string_range: std::ops::Range<usize>,
}

/// Offsets of every string [`strings`] interns, for use as name and file fields.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StringOffsets {
    pub empty: u64,
    pub alpha: u64,
    pub beta: u64,
    pub main: u64,
    pub inline1: u64,
    pub inline2: u64,
    pub alias: u64,
    pub regex: u64,
    pub tmp: u64,
    pub main_c: u64,
    pub foo_h: u64,
}

struct Prologue {
    info: usize,
    strings: usize,
    functions: usize,
}

/// Width of the string and address-info offsets `version` stores.
///
/// GSYM v2 widens both from four bytes to eight; they are never different from
/// each other, which is why one function answers for both.
pub(crate) fn offset_width(version: GsymVersion) -> usize {
    if version == GsymVersion::V1 { 4 } else { 8 }
}

/// Offsets of every string the shared fixture string table interns.
pub(crate) fn string_offsets() -> StringOffsets {
    strings().1
}

fn strings() -> (Vec<u8>, StringOffsets) {
    let mut data = vec![0];
    let mut add = |value: &[u8]| {
        let offset = as_u64(data.len());
        data.extend_from_slice(value);
        data.push(0);
        offset
    };
    let offsets = StringOffsets {
        empty: 0,
        alpha: add(b"alpha"),
        beta: add(b"beta"),
        main: add(b"main"),
        inline1: add(b"inline1"),
        inline2: add(b"inline2"),
        alias: add(b"alias"),
        regex: add(b"^callee$"),
        tmp: add(b"/tmp"),
        main_c: add(b"main.c"),
        foo_h: add(b"foo.h"),
    };
    (data, offsets)
}

/// Byte offset of the name field within a function record, which opens with a
/// four-byte size. [`Fixture::name_slots`] is derived from this.
const RECORD_NAME_OFFSET: usize = 4;

fn encode_function(function: &RawFunction, version: GsymVersion, order: ByteOrder) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(&order.u32(function.size));
    write_uint(&mut output, function.name, offset_width(version), order);
    for (kind, payload) in &function.records {
        output.extend_from_slice(&order.u32(*kind));
        output.extend_from_slice(&order.u32(as_u32(payload.len())));
        output.extend_from_slice(payload);
    }
    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&order.u32(0));
    output
}

/// Encode a complete GSYM image holding `functions` and `files`.
pub(crate) fn build(
    version: GsymVersion,
    order: ByteOrder,
    functions: &[RawFunction],
    files: &[(u64, u64)],
) -> Fixture {
    let base = functions
        .iter()
        .map(|function| function.address)
        .min()
        .unwrap_or(0x1000);
    let (string_data, _) = strings();
    let width = offset_width(version);
    let mut output = Vec::new();

    let prologue = if version == GsymVersion::V1 {
        write_v1_prologue(&mut output, order, base, functions, files, &string_data)
    } else {
        write_v2_prologue(&mut output, order, base, functions, files, &string_data)
    };

    let function_offsets = write_functions(&mut output, order, version, &prologue, functions);
    if version != GsymVersion::V1 {
        // The fifth directory entry is FunctionInfo: type(4), offset(8), size(8).
        let size_slot = 20 + 4 * V2_DIRECTORY_ENTRY_SIZE + 12;
        let size = output.len() - prologue.functions;
        patch_uint(&mut output, size_slot, as_u64(size), 8, order);
    }

    Fixture {
        address_info_slots: (0..functions.len())
            .map(|index| prologue.info + index * width)
            .collect(),
        name_slots: function_offsets
            .iter()
            .map(|offset| offset + RECORD_NAME_OFFSET)
            .collect(),
        string_range: prologue.strings..prologue.functions,
        bytes: output,
    }
}

fn write_v1_prologue(
    output: &mut Vec<u8>,
    order: ByteOrder,
    base: u64,
    functions: &[RawFunction],
    files: &[(u64, u64)],
    string_data: &[u8],
) -> Prologue {
    const STRING_OFFSET_SLOT: usize = 20;
    const WIDTH: usize = 4;

    output.extend_from_slice(&order.u32(MAGIC));
    output.extend_from_slice(&order.u16(1));
    output.push(ADDRESS_WIDTH);
    output.push(4); // build-id length
    output.extend_from_slice(&order.u64(base));
    output.extend_from_slice(&order.u32(as_u32(functions.len())));
    output.extend_from_slice(&order.u32(0)); // string offset, patched below
    output.extend_from_slice(&order.u32(as_u32(string_data.len())));
    output.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    // The header size is already a multiple of the address width, so the
    // address table follows immediately.
    output.resize(V1_HEADER_SIZE, 0);
    write_address_offsets(output, order, base, functions);
    align(output, 4);

    let info_start = output.len();
    output.resize(info_start + functions.len() * WIDTH, 0);
    write_file_table(output, order, WIDTH, files);

    let string_start = output.len();
    output.extend_from_slice(string_data);
    patch_uint(output, STRING_OFFSET_SLOT, as_u64(string_start), 4, order);
    align(output, 4);

    Prologue {
        info: info_start,
        strings: string_start,
        functions: output.len(),
    }
}

fn write_v2_prologue(
    output: &mut Vec<u8>,
    order: ByteOrder,
    base: u64,
    functions: &[RawFunction],
    files: &[(u64, u64)],
    string_data: &[u8],
) -> Prologue {
    const WIDTH: usize = 8;

    let address_start = 20 + V2_DIRECTORY_ENTRIES * V2_DIRECTORY_ENTRY_SIZE;
    let info_start = (address_start + functions.len() * address_width()).next_multiple_of(8);
    let file_start = (info_start + functions.len() * WIDTH).next_multiple_of(4);
    let file_size = 4 + files.len() * WIDTH * 2;
    let string_start = file_start + file_size;
    let function_start = (string_start + string_data.len()).next_multiple_of(4);

    output.extend_from_slice(&order.u32(MAGIC));
    output.extend_from_slice(&order.u16(2));
    output.push(ADDRESS_WIDTH);
    output.push(0); // default NUL-terminated string table
    output.extend_from_slice(&order.u64(base));
    output.extend_from_slice(&order.u32(as_u32(functions.len())));

    for (kind, offset, size) in [
        (1, address_start, functions.len() * address_width()),
        (2, info_start, functions.len() * WIDTH),
        (4, file_start, file_size),
        (3, string_start, string_data.len()),
        (5, function_start, 1),
        (0, 0, 0),
    ] {
        output.extend_from_slice(&order.u32(kind));
        write_offset(output, offset, order);
        write_offset(output, size, order);
    }

    output.resize(address_start, 0);
    write_address_offsets(output, order, base, functions);
    output.resize(file_start, 0);
    write_file_table(output, order, WIDTH, files);
    output.resize(string_start, 0);
    output.extend_from_slice(string_data);
    output.resize(function_start, 0);

    Prologue {
        info: info_start,
        strings: string_start,
        functions: function_start,
    }
}

fn write_functions(
    output: &mut Vec<u8>,
    order: ByteOrder,
    version: GsymVersion,
    prologue: &Prologue,
    functions: &[RawFunction],
) -> Vec<usize> {
    let width = offset_width(version);
    let mut function_offsets = Vec::with_capacity(functions.len());
    for (index, function) in functions.iter().enumerate() {
        let offset = output.len();
        function_offsets.push(offset);
        // v1 stores whole-file offsets; v2 stores offsets into FunctionInfo.
        let stored = if version == GsymVersion::V1 {
            offset
        } else {
            offset - prologue.functions
        };
        patch_uint(
            output,
            prologue.info + index * width,
            as_u64(stored),
            width,
            order,
        );
        output.extend_from_slice(&encode_function(function, version, order));
    }
    function_offsets
}

fn write_address_offsets(
    output: &mut Vec<u8>,
    order: ByteOrder,
    base: u64,
    functions: &[RawFunction],
) {
    for function in functions {
        write_uint(output, function.address - base, address_width(), order);
    }
}

fn write_file_table(output: &mut Vec<u8>, order: ByteOrder, width: usize, files: &[(u64, u64)]) {
    output.extend_from_slice(&order.u32(as_u32(files.len())));
    for &(directory, basename) in files {
        write_uint(output, directory, width, order);
        write_uint(output, basename, width, order);
    }
}

/// The two functions every basic fixture carries: `alpha` and a zero-sized `beta`.
pub(crate) fn basic_functions(offsets: StringOffsets) -> Vec<RawFunction> {
    vec![
        RawFunction {
            address: 0x1000,
            size: 0x10,
            name: offsets.alpha,
            records: Vec::new(),
        },
        RawFunction {
            address: 0x1020,
            size: 0,
            name: offsets.beta,
            records: Vec::new(),
        },
    ]
}

/// A line table placing one row at offset `0x12`, file index 2, line 20.
pub(crate) fn line_table() -> Vec<u8> {
    let mut output = Vec::new();
    sleb(&mut output, 0);
    sleb(&mut output, 0);
    uleb(&mut output, 20);
    output.push(1); // SetFile.
    uleb(&mut output, 2);
    output.push(2); // AdvancePC, emitting the first row.
    uleb(&mut output, 0x12);
    output.push(0);
    output
}

/// An inline tree of `main` -> `inline1` -> `inline2` over `[0x1000, 0x1100)`.
pub(crate) fn inline_info(
    offsets: StringOffsets,
    version: GsymVersion,
    order: ByteOrder,
) -> Vec<u8> {
    let width = offset_width(version);
    let mut output = Vec::new();
    // main [0x1000, 0x1100), with children
    uleb(&mut output, 1);
    uleb(&mut output, 0);
    uleb(&mut output, 0x100);
    output.push(1);
    write_uint(&mut output, offsets.main, width, order);
    uleb(&mut output, 0);
    uleb(&mut output, 0);
    // inline1 [0x1010, 0x1020), with child
    uleb(&mut output, 1);
    uleb(&mut output, 0x10);
    uleb(&mut output, 0x10);
    output.push(1);
    write_uint(&mut output, offsets.inline1, width, order);
    uleb(&mut output, 1);
    uleb(&mut output, 6);
    // inline2 [0x1012, 0x1014), relative to inline1 start
    uleb(&mut output, 1);
    uleb(&mut output, 2);
    uleb(&mut output, 2);
    output.push(0);
    write_uint(&mut output, offsets.inline2, width, order);
    uleb(&mut output, 2);
    uleb(&mut output, 33);
    uleb(&mut output, 0); // inline1 child terminator
    uleb(&mut output, 0); // root child terminator
    output
}

/// One call site at offset `0x12` matching the `^callee$` pattern.
pub(crate) fn call_sites(
    offsets: StringOffsets,
    version: GsymVersion,
    order: ByteOrder,
) -> Vec<u8> {
    let width = offset_width(version);
    let mut output = Vec::new();
    output.extend_from_slice(&order.u32(1));
    output.extend_from_slice(&order.u64(0x12));
    output.push(1);
    output.extend_from_slice(&order.u32(1));
    write_uint(&mut output, offsets.regex, width, order);
    output
}

/// A merged-function record aliasing the enclosing range under the name `alias`.
pub(crate) fn merged(offsets: StringOffsets, version: GsymVersion, order: ByteOrder) -> Vec<u8> {
    let alias = RawFunction {
        address: 0x1000,
        size: 0x100,
        name: offsets.alias,
        records: Vec::new(),
    };
    let encoded = encode_function(&alias, version, order);
    let mut output = Vec::new();
    output.extend_from_slice(&order.u32(1));
    output.extend_from_slice(&order.u32(as_u32(encoded.len())));
    output.extend_from_slice(&encoded);
    output
}

fn address_width() -> usize {
    usize::from(ADDRESS_WIDTH)
}

fn as_u32(value: usize) -> u32 {
    u32::try_from(value).expect("fixture offsets stay well below u32::MAX")
}

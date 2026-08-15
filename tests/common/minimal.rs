//! The smallest hand-written GSYM images the reader has to accept.
//!
//! Both images hold the same two functions -- `alpha` at `0x1000..0x1010` and a
//! zero-sized `beta` at `0x1020` -- with one-byte address offsets and no source
//! files, which also makes them cheap truncation and memory-mapping fodder.
//! Reach for `crate::fixture` instead when a test needs line tables, inline
//! trees or call sites.

use crate::bytes::{ByteOrder, align, as_u64, patch_uint, write_offset, write_uint};

const MAGIC: u32 = 0x4753_594d;
const BASE: u64 = 0x1000;
const STRINGS: &[u8] = b"\0alpha\0beta\0";

/// Encode the fixture as GSYM v1, whose header carries the table offsets.
pub(crate) fn minimal_v1(order: ByteOrder) -> Vec<u8> {
    const HEADER_SIZE: usize = 48;

    let mut output = Vec::with_capacity(128);
    output.extend_from_slice(&order.u32(MAGIC));
    output.extend_from_slice(&order.u16(1));
    output.push(1); // one-byte address offsets
    output.push(4); // build-id length
    output.extend_from_slice(&order.u64(BASE));
    output.extend_from_slice(&order.u32(2));
    let string_offset_fixup = output.len();
    output.extend_from_slice(&order.u32(0));
    let string_size_fixup = output.len();
    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    output.resize(HEADER_SIZE, 0);

    output.extend_from_slice(&[0, 0x20]);
    align(&mut output, 4);
    let function_offsets = output.len();
    output.resize(output.len() + 8, 0);

    output.extend_from_slice(&order.u32(0));

    let string_offset = output.len();
    output.extend_from_slice(STRINGS);
    align(&mut output, 4);

    let alpha_offset = output.len();
    output.extend_from_slice(&order.u32(0x10));
    output.extend_from_slice(&order.u32(1));
    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&order.u32(0));

    let beta_offset = output.len();
    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&order.u32(7));
    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&order.u32(0));

    for (slot, value) in [
        (string_offset_fixup, string_offset),
        (string_size_fixup, STRINGS.len()),
        (function_offsets, alpha_offset),
        (function_offsets + 4, beta_offset),
    ] {
        patch_uint(&mut output, slot, as_u64(value), 4, order);
    }
    output
}

/// Encode the same image with GSYM v2's 64-bit section directory.
pub(crate) fn minimal_v2(order: ByteOrder) -> Vec<u8> {
    const DIRECTORY_ENTRIES: usize = 6;

    let directory_end = 20 + DIRECTORY_ENTRIES * 20;
    let address_offset = directory_end;
    let address_info_offset = (address_offset + 2).next_multiple_of(8);
    let file_offset = (address_info_offset + 16).next_multiple_of(4);
    let string_offset = file_offset + 4;
    let function_offset = (string_offset + STRINGS.len()).next_multiple_of(4);
    let function_size = 40;

    let mut output = Vec::with_capacity(function_offset + function_size);
    output.extend_from_slice(&order.u32(MAGIC));
    output.extend_from_slice(&order.u16(2));
    output.push(1);
    output.push(0); // default NUL-terminated string table
    output.extend_from_slice(&order.u64(BASE));
    output.extend_from_slice(&order.u32(2));

    for (kind, offset, size) in [
        (1, address_offset, 2),
        (2, address_info_offset, 16),
        (4, file_offset, 4),
        (3, string_offset, STRINGS.len()),
        (5, function_offset, function_size),
        (0, 0, 0),
    ] {
        output.extend_from_slice(&order.u32(kind));
        write_offset(&mut output, offset, order);
        write_offset(&mut output, size, order);
    }

    output.resize(address_offset, 0);
    output.extend_from_slice(&[0, 0x20]);
    output.resize(address_info_offset, 0);
    write_uint(&mut output, 0, 8, order);
    write_uint(&mut output, 20, 8, order);
    output.resize(file_offset, 0);
    output.extend_from_slice(&order.u32(0));
    output.resize(string_offset, 0);
    output.extend_from_slice(STRINGS);
    output.resize(function_offset, 0);

    output.extend_from_slice(&order.u32(0x10));
    output.extend_from_slice(&order.u64(1));
    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&order.u32(0));

    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&order.u64(7));
    output.extend_from_slice(&order.u32(0));
    output.extend_from_slice(&order.u32(0));

    output
}

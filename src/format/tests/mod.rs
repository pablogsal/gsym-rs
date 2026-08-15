mod cursor_leb;
mod function_records;
mod headers_layout;
mod line_program;

use crate::Endian;

const ENDIANS: [Endian; 2] = [Endian::Little, Endian::Big];

/// Overwrites `value.len()` bytes at `at`, panicking when they do not fit.
fn patch_bytes(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes
        .split_at_mut(at)
        .1
        .split_at_mut(value.len())
        .0
        .copy_from_slice(value);
}

/// Overwrites the single byte at `at`.
fn patch_byte(bytes: &mut [u8], at: usize, value: u8) {
    patch_bytes(bytes, at, &[value]);
}

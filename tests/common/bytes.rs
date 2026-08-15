//! Byte-level encoding primitives for the hand-written GSYM fixtures.

/// Byte order a fixture is emitted in.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    pub(crate) const fn u16(self, value: u16) -> [u8; 2] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }

    pub(crate) const fn u32(self, value: u32) -> [u8; 4] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }

    pub(crate) const fn u64(self, value: u64) -> [u8; 8] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }
}

/// Zero-pad `output` up to the next multiple of `alignment`.
pub(crate) fn align(output: &mut Vec<u8>, alignment: usize) {
    output.resize(output.len().next_multiple_of(alignment), 0);
}

/// Append the `width` significant bytes of `value` in `order`.
pub(crate) fn write_uint(output: &mut Vec<u8>, value: u64, width: usize, order: ByteOrder) {
    let bytes = order.u64(value);
    match order {
        ByteOrder::Little => output.extend_from_slice(&bytes[..width]),
        ByteOrder::Big => output.extend_from_slice(&bytes[8 - width..]),
    }
}

/// Append `offset` as the 64-bit field a GSYM v2 section directory stores.
///
/// Directory offsets and sizes are computed as `usize`, so this keeps the
/// widening in one place instead of at every call site.
pub(crate) fn write_offset(output: &mut Vec<u8>, offset: usize, order: ByteOrder) {
    output.extend_from_slice(&order.u64(as_u64(offset)));
}

/// Overwrite the `width` bytes at `offset` with `value` in `order`.
pub(crate) fn patch_uint(
    output: &mut [u8],
    offset: usize,
    value: u64,
    width: usize,
    order: ByteOrder,
) {
    let bytes = order.u64(value);
    let source = match order {
        ByteOrder::Little => &bytes[..width],
        ByteOrder::Big => &bytes[8 - width..],
    };
    output[offset..offset + width].copy_from_slice(source);
}

/// Widen a fixture offset or length to the `u64` the wire format stores.
pub(crate) fn as_u64(value: usize) -> u64 {
    u64::try_from(value).expect("fixture offsets stay well below u64::MAX")
}

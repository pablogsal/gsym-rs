use crate::endian::{Cursor, Encoder};
use crate::error::{Error, Result};

/// Widest value either LEB128 reader can produce.
const MAXIMUM_BITS: u8 = 64;

/// Reads an unsigned LEB128 value that must fit in `u64`.
#[inline]
pub(crate) fn read_uleb(cursor: &mut Cursor<'_>) -> Result<u64> {
    read_uleb_bounded(cursor, MAXIMUM_BITS)
}

/// Reads an unsigned LEB128 value that must fit in `bits` bits.
#[inline]
pub(crate) fn read_uleb_bounded(cursor: &mut Cursor<'_>, bits: u8) -> Result<u64> {
    if bits == 0 || bits > MAXIMUM_BITS {
        return Err(Error::OutOfRange {
            field: "ULEB128 bit width",
            value: u64::from(bits),
            max: u64::from(MAXIMUM_BITS),
        });
    }
    let start = cursor.position();
    let first = cursor
        .read_u8()
        .map_err(|error| map_uleb_eof(error, start))?;
    if first & 0x80 == 0 {
        return check_uleb_width(u64::from(first), bits, start);
    }

    let mut value = u64::from(first & 0x7f);
    for index in 1..u32::from(bits).div_ceil(7) {
        let byte = cursor
            .read_u8()
            .map_err(|error| map_uleb_eof(error, start))?;
        let shift = index.saturating_mul(7);
        let payload = u64::from(byte & 0x7f);
        let shifted = payload.checked_shl(shift).ok_or(Error::MalformedUleb {
            offset: start,
            reason: "value exceeds u64",
        })?;
        if shifted.checked_shr(shift).unwrap_or(0) != payload {
            return Err(Error::MalformedUleb {
                offset: start,
                reason: "value exceeds u64",
            });
        }
        value |= shifted;
        if byte & 0x80 == 0 {
            return check_uleb_width(value, bits, start);
        }
    }
    Err(Error::MalformedUleb {
        offset: start,
        reason: "sequence is too long",
    })
}

fn check_uleb_width(value: u64, bits: u8, start: usize) -> Result<u64> {
    let maximum = u64::MAX.wrapping_shr(u32::from(MAXIMUM_BITS.saturating_sub(bits)));
    if value > maximum {
        return Err(Error::MalformedUleb {
            offset: start,
            reason: "value exceeds requested bit width",
        });
    }
    Ok(value)
}

/// Reads a signed LEB128 value that must fit in `i64`.
#[inline]
pub(crate) fn read_sleb(cursor: &mut Cursor<'_>) -> Result<i64> {
    read_sleb_bounded(cursor, MAXIMUM_BITS)
}

/// Reads a signed LEB128 value that must fit in `bits` bits.
#[inline]
pub(crate) fn read_sleb_bounded(cursor: &mut Cursor<'_>, bits: u8) -> Result<i64> {
    if bits == 0 || bits > MAXIMUM_BITS {
        return Err(Error::OutOfRange {
            field: "SLEB128 bit width",
            value: u64::from(bits),
            max: u64::from(MAXIMUM_BITS),
        });
    }
    let start = cursor.position();
    let first = cursor
        .read_u8()
        .map_err(|error| map_sleb_eof(error, start))?;
    if first & 0x80 == 0 {
        let value = if first & 0x40 == 0 {
            i128::from(first)
        } else {
            i128::from(first | 0x80).saturating_sub(0x100)
        };
        return check_sleb_width(value, bits, start);
    }

    let mut value = i128::from(first & 0x7f);
    for index in 1..u32::from(bits).div_ceil(7) {
        let byte = cursor
            .read_u8()
            .map_err(|error| map_sleb_eof(error, start))?;
        let shift = index.saturating_mul(7);
        value |= i128::from(byte & 0x7f)
            .checked_shl(shift)
            .ok_or(Error::MalformedSleb {
                offset: start,
                reason: "value exceeds i64",
            })?;
        if byte & 0x80 == 0 {
            let encoded_bits = shift.saturating_add(7);
            if byte & 0x40 != 0 {
                value |= (-1_i128).checked_shl(encoded_bits).unwrap_or(0);
            }
            return check_sleb_width(value, bits, start);
        }
    }
    Err(Error::MalformedSleb {
        offset: start,
        reason: "sequence is too long",
    })
}

fn check_sleb_width(value: i128, bits: u8, start: usize) -> Result<i64> {
    if bits < MAXIMUM_BITS {
        let magnitude = 1_i128.wrapping_shl(u32::from(bits).saturating_sub(1));
        if value < magnitude.wrapping_neg() || value > magnitude.saturating_sub(1) {
            return Err(Error::MalformedSleb {
                offset: start,
                reason: "value exceeds requested bit width",
            });
        }
    }
    i64::try_from(value).map_err(|_| Error::MalformedSleb {
        offset: start,
        reason: "value exceeds i64",
    })
}

fn map_uleb_eof(error: Error, offset: usize) -> Error {
    if matches!(error, Error::UnexpectedEof { .. }) {
        Error::MalformedUleb {
            offset,
            reason: "unterminated sequence",
        }
    } else {
        error
    }
}

fn map_sleb_eof(error: Error, offset: usize) -> Error {
    if matches!(error, Error::UnexpectedEof { .. }) {
        Error::MalformedSleb {
            offset,
            reason: "unterminated sequence",
        }
    } else {
        error
    }
}

pub(crate) fn write_uleb(output: &mut Encoder, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.write_u8(byte);
        if value == 0 {
            break;
        }
    }
}

pub(crate) fn write_sleb(output: &mut Encoder, mut value: i64) {
    loop {
        let byte = value.to_le_bytes()[0] & 0x7f;
        let sign_set = byte & 0x40 != 0;
        value >>= 7;
        let done = (value == 0 && !sign_set) || (value == -1 && sign_set);
        output.write_u8(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

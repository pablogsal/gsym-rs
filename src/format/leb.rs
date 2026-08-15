use crate::endian::{Cursor, Encoder};
use crate::error::{Error, Result};

pub(crate) fn read_uleb(cursor: &mut Cursor<'_>) -> Result<u64> {
    let start = cursor.position();
    let first = cursor
        .read_u8()
        .map_err(|error| map_uleb_eof(error, start))?;
    if first & 0x80 == 0 {
        return Ok(u64::from(first));
    }

    let mut value = u64::from(first & 0x7f);
    for index in 1..10_u32 {
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
            return Ok(value);
        }
    }
    Err(Error::MalformedUleb {
        offset: start,
        reason: "sequence is too long",
    })
}

#[cfg(test)]
pub(crate) fn read_uleb_bounded(cursor: &mut Cursor<'_>, bits: u8) -> Result<u64> {
    if bits == 64 {
        return read_uleb(cursor);
    }
    let start = cursor.position();
    if bits == 0 || bits > 64 {
        return Err(Error::OutOfRange {
            field: "ULEB128 bit width",
            value: u64::from(bits),
            max: 64,
        });
    }

    let maximum = if bits == 64 {
        u64::MAX
    } else {
        1_u64
            .checked_shl(u32::from(bits))
            .unwrap_or(0)
            .saturating_sub(1)
    };
    let maximum_bytes = usize::from(bits).div_ceil(7);
    // Only the final group of a 64-bit value can carry bits past the top.
    let mut value = 0_u64;

    for index in 0..maximum_bytes {
        let byte = cursor.read_u8().map_err(|error| {
            if matches!(error, Error::UnexpectedEof { .. }) {
                Error::MalformedUleb {
                    offset: start,
                    reason: "unterminated sequence",
                }
            } else {
                error
            }
        })?;
        let shift = u32::try_from(index).unwrap_or(u32::MAX).saturating_mul(7);
        let payload = u64::from(byte & 0x7f);
        let shifted = payload.checked_shl(shift).unwrap_or(0);
        if shifted.checked_shr(shift).unwrap_or(0) != payload {
            return Err(Error::MalformedUleb {
                offset: start,
                reason: "value exceeds u64",
            });
        }
        value |= shifted;
        if byte & 0x80 == 0 {
            if value > maximum {
                return Err(Error::MalformedUleb {
                    offset: start,
                    reason: "value exceeds requested bit width",
                });
            }
            return Ok(value);
        }
    }

    Err(Error::MalformedUleb {
        offset: start,
        reason: "sequence is too long",
    })
}

pub(crate) fn read_sleb(cursor: &mut Cursor<'_>) -> Result<i64> {
    let start = cursor.position();
    let first = cursor
        .read_u8()
        .map_err(|error| map_sleb_eof(error, start))?;
    if first & 0x80 == 0 {
        let value = if first & 0x40 == 0 {
            i64::from(first)
        } else {
            i64::from(first | 0x80).saturating_sub(0x100)
        };
        return Ok(value);
    }

    let mut value = i128::from(first & 0x7f);
    for index in 1..10_u32 {
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
            return i64::try_from(value).map_err(|_| Error::MalformedSleb {
                offset: start,
                reason: "value exceeds i64",
            });
        }
    }
    Err(Error::MalformedSleb {
        offset: start,
        reason: "sequence is too long",
    })
}

#[cfg(test)]
pub(crate) fn read_sleb_bounded(cursor: &mut Cursor<'_>, bits: u8) -> Result<i64> {
    if bits == 64 {
        return read_sleb(cursor);
    }
    let start = cursor.position();
    if bits == 0 || bits > 64 {
        return Err(Error::OutOfRange {
            field: "SLEB128 bit width",
            value: u64::from(bits),
            max: 64,
        });
    }

    let maximum_bytes = usize::from(bits).div_ceil(7);
    let mut value = 0_i128;
    for index in 0..maximum_bytes {
        let byte = cursor.read_u8().map_err(|error| {
            if matches!(error, Error::UnexpectedEof { .. }) {
                Error::MalformedSleb {
                    offset: start,
                    reason: "unterminated sequence",
                }
            } else {
                error
            }
        })?;
        let shift = u32::try_from(index).unwrap_or(u32::MAX).saturating_mul(7);
        value |= i128::from(byte & 0x7f).checked_shl(shift).unwrap_or(0);
        if byte & 0x80 == 0 {
            let encoded_bits = shift.saturating_add(7);
            if byte & 0x40 != 0 {
                value |= (-1_i128).checked_shl(encoded_bits).unwrap_or(0);
            }
            let magnitude = 1_i128
                .checked_shl(u32::from(bits).saturating_sub(1))
                .unwrap_or(0);
            let minimum = magnitude.wrapping_neg();
            let maximum = magnitude.saturating_sub(1);
            if value < minimum || value > maximum {
                return Err(Error::MalformedSleb {
                    offset: start,
                    reason: "value exceeds requested bit width",
                });
            }
            return i64::try_from(value).map_err(|_| Error::MalformedSleb {
                offset: start,
                reason: "value exceeds i64",
            });
        }
    }

    Err(Error::MalformedSleb {
        offset: start,
        reason: "sequence is too long",
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

#[cfg(test)]
mod tests {
    use crate::endian::{Cursor, Encoder, Endian};

    use super::{read_sleb, read_uleb, read_uleb_bounded, write_sleb, write_uleb};

    #[test]
    fn leb_examples_round_trip() {
        let unsigned = [0_u64, 1, 127, 128, 624_485, u64::MAX];
        for value in unsigned {
            let mut encoded = Encoder::new(Endian::Little);
            write_uleb(&mut encoded, value);
            let mut cursor = Cursor::new(encoded.as_slice(), Endian::Big);
            assert_eq!(read_uleb(&mut cursor).unwrap(), value);
            assert!(cursor.is_empty());
        }

        let signed = [i64::MIN, -624_485, -65, -64, -1, 0, 63, 64, i64::MAX];
        for value in signed {
            let mut encoded = Encoder::new(Endian::Little);
            write_sleb(&mut encoded, value);
            let mut cursor = Cursor::new(encoded.as_slice(), Endian::Big);
            assert_eq!(read_sleb(&mut cursor).unwrap(), value);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn rejects_unterminated_and_out_of_width_values() {
        let mut unterminated = Cursor::new(&[0x80; 10], Endian::Little);
        assert!(read_uleb(&mut unterminated).is_err());

        let mut too_large = Cursor::new(&[0x80, 0x02], Endian::Little);
        assert!(read_uleb_bounded(&mut too_large, 8).is_err());
    }
}

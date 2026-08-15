//! LEB128 encoders for the hand-written GSYM fixtures.
//!
//! Line tables and inline trees are variable-length encoded, so fixtures that
//! carry them need their own encoder rather than reusing `src/format/leb.rs`.
//! LEB sequences are byte-order independent, so nothing here takes a
//! [`ByteOrder`](super::bytes::ByteOrder).

/// Append `value` as an unsigned LEB128 sequence.
pub(crate) fn uleb(output: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = low_seven_bits(value);
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Append `value` as a signed LEB128 sequence.
pub(crate) fn sleb(output: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = low_seven_bits(value.cast_unsigned());
        let sign = byte & 0x40 != 0;
        value >>= 7;
        let done = (value == 0 && !sign) || (value == -1 && sign);
        output.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

fn low_seven_bits(value: u64) -> u8 {
    u8::try_from(value & 0x7f).expect("a seven-bit mask always fits in a byte")
}

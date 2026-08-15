use crate::error::{Error, Result};

/// Byte order used for fixed-width integers in a GSYM file.
///
/// A file records its own byte order, and both orders can be read on any host.
///
/// Conversion writes the byte order of the ELF image it read. Everything else
/// defaults to [`Little`](Self::Little) and can be changed with
/// [`GsymBuilder::endian`](crate::GsymBuilder::endian) or
/// [`TranscodeOptions`](crate::TranscodeOptions).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endian {
    /// Least-significant byte first.
    Little,
    /// Most-significant byte first.
    Big,
}

impl Endian {
    /// Returns the byte order of the compilation target.
    #[must_use]
    pub const fn native() -> Self {
        if cfg!(target_endian = "little") {
            Self::Little
        } else {
            Self::Big
        }
    }
}

/// Bounds-checked cursor over untrusted binary input.
#[derive(Clone, Debug)]
pub(crate) struct Cursor<'data> {
    bytes: &'data [u8],
    position: usize,
    endian: Endian,
}

impl<'data> Cursor<'data> {
    pub(crate) const fn new(bytes: &'data [u8], endian: Endian) -> Self {
        Self {
            bytes,
            position: 0,
            endian,
        }
    }

    pub(crate) const fn at(bytes: &'data [u8], endian: Endian, position: usize) -> Result<Self> {
        if position > bytes.len() {
            return Err(Error::InvalidOffset {
                offset: position as u64,
                input_len: bytes.len(),
            });
        }
        Ok(Self {
            bytes,
            position,
            endian,
        })
    }

    pub(crate) const fn endian(&self) -> Endian {
        self.endian
    }

    pub(crate) const fn position(&self) -> usize {
        self.position
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub(crate) fn take(&mut self, count: usize) -> Result<&'data [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(Error::Overflow("cursor end offset"))?;
        let Some(bytes) = self.bytes.get(self.position..end) else {
            return Err(Error::UnexpectedEof {
                offset: self.position,
                needed: count,
                remaining: self.remaining(),
            });
        };
        self.position = end;
        Ok(bytes)
    }

    pub(crate) fn take_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let offset = self.position;
        let remaining = self.remaining();
        let bytes = self
            .bytes
            .get(offset..)
            .and_then(<[u8]>::first_chunk::<N>)
            .ok_or(Error::UnexpectedEof {
                offset,
                needed: N,
                remaining,
            })?;
        self.position = offset.saturating_add(N);
        Ok(*bytes)
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8> {
        let [byte] = self.take_array()?;
        Ok(byte)
    }

    pub(crate) fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.take_array()?;
        Ok(match self.endian {
            Endian::Little => u16::from_le_bytes(bytes),
            Endian::Big => u16::from_be_bytes(bytes),
        })
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.take_array()?;
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        })
    }

    pub(crate) fn read_u64(&mut self) -> Result<u64> {
        let bytes = self.take_array()?;
        Ok(match self.endian {
            Endian::Little => u64::from_le_bytes(bytes),
            Endian::Big => u64::from_be_bytes(bytes),
        })
    }

    pub(crate) fn read_uint(&mut self, width: u8) -> Result<u64> {
        match width {
            1 => return self.read_u8().map(u64::from),
            2 => return self.read_u16().map(u64::from),
            4 => return self.read_u32().map(u64::from),
            8 => return self.read_u64(),
            3 | 5..=7 => {}
            _ => {
                return Err(Error::OutOfRange {
                    field: "integer width",
                    value: u64::from(width),
                    max: 8,
                });
            }
        }
        let count = usize::from(width);
        let bytes = self.take(count)?;
        let mut buffer = [0_u8; 8];
        let window = match self.endian {
            Endian::Little => buffer.get_mut(..count),
            Endian::Big => buffer.get_mut(size_of::<u64>().saturating_sub(count)..),
        };
        window
            .ok_or_else(|| Error::OutOfRange {
                field: "integer width",
                value: u64::from(width),
                max: 8,
            })?
            .copy_from_slice(bytes);
        Ok(match self.endian {
            Endian::Little => u64::from_le_bytes(buffer),
            Endian::Big => u64::from_be_bytes(buffer),
        })
    }
}

/// Checked binary output buffer with endian-aware fixed-width writes.
#[derive(Clone, Debug)]
pub(crate) struct Encoder {
    bytes: Vec<u8>,
    endian: Endian,
}

impl Encoder {
    #[cfg(test)]
    pub(crate) const fn new(endian: Endian) -> Self {
        Self {
            bytes: Vec::new(),
            endian,
        }
    }

    pub(crate) fn with_capacity(endian: Endian, capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
            endian,
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.bytes.len()
    }

    #[cfg(test)]
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&match self.endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        });
    }

    pub(crate) fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&match self.endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        });
    }

    pub(crate) fn write_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&match self.endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        });
    }

    pub(crate) fn write_uint(&mut self, value: u64, width: u8) -> Result<()> {
        if !(1..=8).contains(&width) {
            return Err(Error::OutOfRange {
                field: "integer width",
                value: u64::from(width),
                max: 8,
            });
        }
        let count = usize::from(width);
        if width < 8 {
            let max = u64::MAX
                .checked_shr(u32::from(8_u8.saturating_sub(width)).saturating_mul(8))
                .unwrap_or(u64::MAX);
            if value > max {
                return Err(Error::OutOfRange {
                    field: "fixed-width integer",
                    value,
                    max,
                });
            }
        }
        let bytes = match self.endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        let window = match self.endian {
            Endian::Little => bytes.get(..count),
            Endian::Big => bytes.get(size_of::<u64>().saturating_sub(count)..),
        };
        self.bytes
            .extend_from_slice(window.ok_or_else(|| Error::OutOfRange {
                field: "integer width",
                value: u64::from(width),
                max: 8,
            })?);
        Ok(())
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    pub(crate) fn align_to(&mut self, alignment: usize) -> Result<()> {
        if alignment == 0 {
            return Err(Error::InvalidAlignment(alignment));
        }
        let remainder = self.bytes.len().checked_rem(alignment).unwrap_or(0);
        if remainder != 0 {
            let padding = alignment.saturating_sub(remainder);
            let new_len = self
                .bytes
                .len()
                .checked_add(padding)
                .ok_or(Error::Overflow("aligned output length"))?;
            self.bytes.resize(new_len, 0);
        }
        Ok(())
    }

    pub(crate) fn patch_u32(&mut self, offset: usize, value: u32) -> Result<()> {
        let bytes = match self.endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        let end = offset
            .checked_add(bytes.len())
            .ok_or(Error::Overflow("patch end offset"))?;
        let remaining = self.bytes.len().saturating_sub(offset);
        let destination = self
            .bytes
            .get_mut(offset..end)
            .ok_or(Error::UnexpectedEof {
                offset,
                needed: bytes.len(),
                remaining,
            })?;
        destination.copy_from_slice(&bytes);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn patch_uint(&mut self, offset: usize, value: u64, width: u8) -> Result<()> {
        let mut encoded = Self::new(self.endian);
        encoded.write_uint(value, width)?;
        let end = offset
            .checked_add(usize::from(width))
            .ok_or(Error::Overflow("patch end offset"))?;
        let remaining = self.bytes.len().saturating_sub(offset);
        let destination = self
            .bytes
            .get_mut(offset..end)
            .ok_or_else(|| Error::UnexpectedEof {
                offset,
                needed: usize::from(width),
                remaining,
            })?;
        destination.copy_from_slice(encoded.as_slice());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, Encoder, Endian};

    #[test]
    fn fixed_width_round_trip_both_endians() {
        for endian in [Endian::Little, Endian::Big] {
            let mut output = Encoder::new(endian);
            output.write_u8(0x12);
            output.write_u16(0x3456);
            output.write_u32(0x789a_bcde);
            output.write_u64(0x0123_4567_89ab_cdef);
            output.write_uint(0xa1_b2_c3, 3).unwrap();

            let mut input = Cursor::new(output.as_slice(), endian);
            assert_eq!(input.read_u8().unwrap(), 0x12);
            assert_eq!(input.read_u16().unwrap(), 0x3456);
            assert_eq!(input.read_u32().unwrap(), 0x789a_bcde);
            assert_eq!(input.read_u64().unwrap(), 0x0123_4567_89ab_cdef);
            assert_eq!(input.read_uint(3).unwrap(), 0xa1_b2_c3);
            assert!(input.is_empty());
        }
    }

    #[test]
    fn bounds_and_alignment_are_checked() {
        let mut input = Cursor::new(&[1, 2], Endian::Little);
        assert!(input.read_u32().is_err());

        let mut output = Encoder::new(Endian::Little);
        output.write_u8(1);
        output.align_to(4).unwrap();
        assert_eq!(output.as_slice(), &[1, 0, 0, 0]);
        assert!(output.patch_u32(1, 7).is_err());
    }
}

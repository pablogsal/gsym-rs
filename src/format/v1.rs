use std::ops::Range;

use crate::endian::{Cursor, Encoder, Endian};
use crate::error::{Error, Result};
use crate::format::{GSYM_CIGAM, GSYM_MAGIC, align_up};

pub(crate) const VERSION: u16 = 1;
pub(crate) const HEADER_SIZE: usize = 48;
pub(crate) const MAX_UUID_SIZE: usize = 20;
pub(crate) const ADDRESS_INFO_OFFSET_SIZE: u8 = 4;
pub(crate) const STRING_OFFSET_SIZE: u8 = 4;
pub(crate) const FILE_ENTRY_SIZE: usize = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FixedUuid {
    bytes: [u8; MAX_UUID_SIZE],
    len: u8,
}

impl FixedUuid {
    pub(crate) fn new(uuid: &[u8]) -> Result<Self> {
        if uuid.len() > MAX_UUID_SIZE {
            return Err(Error::V1BuildIdTooLong { size: uuid.len() });
        }
        let mut bytes = [0; MAX_UUID_SIZE];
        bytes
            .get_mut(..uuid.len())
            .ok_or(Error::V1BuildIdTooLong { size: uuid.len() })?
            .copy_from_slice(uuid);
        Ok(Self {
            bytes,
            len: u8::try_from(uuid.len())
                .map_err(|_| Error::V1BuildIdTooLong { size: uuid.len() })?,
        })
    }

    pub(crate) const fn as_slice(&self) -> &[u8] {
        self.bytes.split_at(self.len as usize).0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub(crate) address_offset_size: u8,
    pub(crate) base_address: u64,
    pub(crate) address_count: u32,
    pub(crate) string_table_offset: u32,
    pub(crate) string_table_size: u32,
    pub(crate) uuid: FixedUuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    pub(crate) address_offsets: Range<usize>,
    pub(crate) address_info_offsets: Range<usize>,
    pub(crate) file_table_offset: usize,
    pub(crate) string_table: Range<usize>,
    pub(crate) function_info_offset: usize,
}

pub(crate) fn detect_endian(bytes: &[u8]) -> Result<Endian> {
    let magic_bytes: [u8; 4] = bytes
        .get(..4)
        .and_then(|prefix| <[u8; 4]>::try_from(prefix).ok())
        .ok_or(Error::UnexpectedEof {
            offset: 0,
            needed: 4,
            remaining: bytes.len(),
        })?;
    let host_value = u32::from_ne_bytes(magic_bytes);
    if host_value == GSYM_MAGIC {
        Ok(Endian::native())
    } else if host_value == GSYM_CIGAM {
        Ok(match Endian::native() {
            Endian::Little => Endian::Big,
            Endian::Big => Endian::Little,
        })
    } else {
        Err(Error::InvalidMagic(host_value))
    }
}

impl Header {
    pub(crate) fn decode(bytes: &[u8]) -> Result<(Self, Endian)> {
        let endian = detect_endian(bytes)?;
        let mut cursor = Cursor::new(bytes, endian);
        let magic = cursor.read_u32()?;
        if magic != GSYM_MAGIC {
            return Err(Error::InvalidMagic(magic));
        }
        let version = cursor.read_u16()?;
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let address_offset_size = cursor.read_u8()?;
        validate_address_offset_size(address_offset_size)?;
        let uuid_size = usize::from(cursor.read_u8()?);
        if uuid_size > MAX_UUID_SIZE {
            return Err(Error::InvalidUuidSize(uuid_size));
        }
        let base_address = cursor.read_u64()?;
        let address_count = cursor.read_u32()?;
        let string_table_offset = cursor.read_u32()?;
        let string_table_size = cursor.read_u32()?;
        let uuid_storage = cursor.take(MAX_UUID_SIZE)?;
        let uuid = FixedUuid::new(
            uuid_storage
                .get(..uuid_size)
                .ok_or(Error::InvalidFormat("v1 build ID is truncated"))?,
        )?;
        Ok((
            Self {
                address_offset_size,
                base_address,
                address_count,
                string_table_offset,
                string_table_size,
                uuid,
            },
            endian,
        ))
    }

    pub(crate) fn encode(&self, endian: Endian) -> Result<Vec<u8>> {
        validate_address_offset_size(self.address_offset_size)?;
        let uuid = self.uuid.as_slice();
        let mut output = Encoder::with_capacity(endian, HEADER_SIZE);
        output.write_u32(GSYM_MAGIC);
        output.write_u16(VERSION);
        output.write_u8(self.address_offset_size);
        output.write_u8(self.uuid.len);
        output.write_u64(self.base_address);
        output.write_u32(self.address_count);
        output.write_u32(self.string_table_offset);
        output.write_u32(self.string_table_size);
        output.write_bytes(uuid);
        output.write_bytes(
            [0; MAX_UUID_SIZE]
                .get(uuid.len()..)
                .ok_or(Error::V1BuildIdTooLong { size: uuid.len() })?,
        );
        debug_assert_eq!(output.len(), HEADER_SIZE);
        Ok(output.into_inner())
    }

    pub(crate) fn layout(&self, input_len: usize) -> Result<Layout> {
        let address_offsets_start = align_up(HEADER_SIZE, usize::from(self.address_offset_size))
            .ok_or(Error::Overflow("v1 address-table alignment"))?;
        let address_offsets_size = usize::try_from(self.address_count)
            .map_err(|_| Error::Overflow("v1 address count"))?
            .checked_mul(usize::from(self.address_offset_size))
            .ok_or(Error::Overflow("v1 address-table size"))?;
        let address_offsets_end = address_offsets_start
            .checked_add(address_offsets_size)
            .ok_or(Error::Overflow("v1 address-table end"))?;
        let address_info_start =
            align_up(address_offsets_end, 4).ok_or(Error::Overflow("v1 address-info alignment"))?;
        let address_info_size = usize::try_from(self.address_count)
            .map_err(|_| Error::Overflow("v1 address count"))?
            .checked_mul(usize::from(ADDRESS_INFO_OFFSET_SIZE))
            .ok_or(Error::Overflow("v1 address-info table size"))?;
        let address_info_end = address_info_start
            .checked_add(address_info_size)
            .ok_or(Error::Overflow("v1 address-info table end"))?;
        let file_table_offset =
            align_up(address_info_end, 4).ok_or(Error::Overflow("v1 file-table alignment"))?;
        let string_start = usize::try_from(self.string_table_offset)
            .map_err(|_| Error::Overflow("v1 string-table offset"))?;
        let string_end = string_start
            .checked_add(
                usize::try_from(self.string_table_size)
                    .map_err(|_| Error::Overflow("v1 string-table size"))?,
            )
            .ok_or(Error::Overflow("v1 string-table end"))?;
        if file_table_offset > string_start {
            return Err(Error::InvalidFormat(
                "v1 string table overlaps fixed tables",
            ));
        }
        if string_end > input_len {
            return Err(Error::SectionOutOfBounds {
                section_type: 3,
                offset: u64::from(self.string_table_offset),
                size: u64::from(self.string_table_size),
                input_len,
            });
        }
        Ok(Layout {
            address_offsets: address_offsets_start..address_offsets_end,
            address_info_offsets: address_info_start..address_info_end,
            file_table_offset,
            string_table: string_start..string_end,
            function_info_offset: string_end,
        })
    }
}

pub(crate) const fn validate_address_offset_size(size: u8) -> Result<()> {
    if matches!(size, 1 | 2 | 4 | 8) {
        Ok(())
    } else {
        Err(Error::InvalidAddressOffsetSize {
            version: VERSION,
            size,
        })
    }
}

pub(crate) fn file_table_size(file_count: u32) -> Result<usize> {
    super::file_table_size(file_count, FILE_ENTRY_SIZE, "v1 file-table size")
}

#[cfg(test)]
mod tests {
    use crate::endian::Endian;

    use super::{FixedUuid, HEADER_SIZE, Header};

    #[test]
    fn header_round_trip_and_magic_bytes() {
        let header = Header {
            address_offset_size: 2,
            base_address: 0x0040_0000,
            address_count: 11,
            string_table_offset: 136,
            string_table_size: 167,
            uuid: FixedUuid::new(&(1..=20).collect::<Vec<_>>()).unwrap(),
        };
        for endian in [Endian::Little, Endian::Big] {
            let bytes = header.encode(endian).unwrap();
            assert_eq!(bytes.len(), HEADER_SIZE);
            assert_eq!(
                bytes.get(..4).unwrap(),
                if endian == Endian::Little {
                    b"MYSG"
                } else {
                    b"GSYM"
                }
            );
            assert_eq!(Header::decode(&bytes).unwrap(), (header.clone(), endian));
        }
    }

    #[test]
    fn layout_matches_v1_wire_order() {
        let header = Header {
            address_offset_size: 2,
            base_address: 0,
            address_count: 11,
            string_table_offset: 136,
            string_table_size: 167,
            uuid: FixedUuid::default(),
        };
        let layout = header.layout(400).unwrap();
        assert_eq!(layout.address_offsets, 48..70);
        assert_eq!(layout.address_info_offsets, 72..116);
        assert_eq!(layout.file_table_offset, 116);
        assert_eq!(layout.string_table, 136..303);
        assert_eq!(layout.function_info_offset, 303);
    }
}

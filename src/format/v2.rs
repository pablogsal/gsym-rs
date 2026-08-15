use std::collections::BTreeSet;
use std::ops::Range;

use crate::endian::{Cursor, Encoder, Endian};
use crate::error::{Error, Result};
use crate::format::{GSYM_MAGIC, align_up};

use super::v1::detect_endian;

pub(crate) const VERSION: u16 = 2;
pub(crate) const HEADER_SIZE: usize = 20;
pub(crate) const GLOBAL_DATA_SIZE: usize = 20;
pub(crate) const ADDRESS_INFO_OFFSET_SIZE: u8 = 8;
pub(crate) const STRING_OFFSET_SIZE: u8 = 8;
pub(crate) const FILE_ENTRY_SIZE: usize = 16;
pub(crate) const STRING_TABLE_ENCODING_DEFAULT: u8 = 0;

pub(crate) const GLOBAL_END: u32 = 0;
pub(crate) const GLOBAL_ADDRESS_OFFSETS: u32 = 1;
pub(crate) const GLOBAL_ADDRESS_INFO_OFFSETS: u32 = 2;
pub(crate) const GLOBAL_STRING_TABLE: u32 = 3;
pub(crate) const GLOBAL_FILE_TABLE: u32 = 4;
pub(crate) const GLOBAL_FUNCTION_INFO: u32 = 5;
pub(crate) const GLOBAL_UUID: u32 = 6;

const REQUIRED_TYPES: [u32; 5] = [
    GLOBAL_ADDRESS_OFFSETS,
    GLOBAL_ADDRESS_INFO_OFFSETS,
    GLOBAL_STRING_TABLE,
    GLOBAL_FILE_TABLE,
    GLOBAL_FUNCTION_INFO,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub(crate) address_offset_size: u8,
    pub(crate) string_table_encoding: u8,
    pub(crate) base_address: u64,
    pub(crate) address_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GlobalData {
    pub(crate) section_type: u32,
    pub(crate) file_offset: u64,
    pub(crate) file_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Directory {
    pub(crate) entries: Box<[GlobalData]>,
    pub(crate) end_offset: usize,
}

impl Directory {
    pub(crate) fn get(&self, section_type: u32) -> Option<GlobalData> {
        self.entries
            .iter()
            .copied()
            .find(|entry| entry.section_type == section_type)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WriteLayout {
    pub(crate) directory: Box<[GlobalData]>,
    pub(crate) directory_end: usize,
    pub(crate) uuid: Option<Range<usize>>,
    pub(crate) address_offsets: Range<usize>,
    pub(crate) address_info_offsets: Range<usize>,
    pub(crate) file_table: Range<usize>,
    pub(crate) string_table: Range<usize>,
    pub(crate) function_info: Range<usize>,
    pub(crate) file_size: usize,
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
        let string_table_encoding = cursor.read_u8()?;
        if string_table_encoding != STRING_TABLE_ENCODING_DEFAULT {
            return Err(Error::UnsupportedStringTableEncoding(string_table_encoding));
        }
        let base_address = cursor.read_u64()?;
        let address_count = cursor.read_u32()?;
        Ok((
            Self {
                address_offset_size,
                string_table_encoding,
                base_address,
                address_count,
            },
            endian,
        ))
    }

    pub(crate) fn encode(&self, endian: Endian) -> Result<Vec<u8>> {
        validate_address_offset_size(self.address_offset_size)?;
        if self.string_table_encoding != STRING_TABLE_ENCODING_DEFAULT {
            return Err(Error::UnsupportedStringTableEncoding(
                self.string_table_encoding,
            ));
        }
        let mut output = Encoder::with_capacity(endian, HEADER_SIZE);
        output.write_u32(GSYM_MAGIC);
        output.write_u16(VERSION);
        output.write_u8(self.address_offset_size);
        output.write_u8(self.string_table_encoding);
        output.write_u64(self.base_address);
        output.write_u32(self.address_count);
        debug_assert_eq!(output.len(), HEADER_SIZE);
        Ok(output.into_inner())
    }

    pub(crate) fn write_layout(
        &self,
        uuid_size: usize,
        file_count: u32,
        string_table_size: usize,
        function_info_size: usize,
    ) -> Result<WriteLayout> {
        let has_uuid = uuid_size != 0;
        let directory_entry_count = 5_usize
            .checked_add(usize::from(has_uuid))
            .and_then(|count| count.checked_add(1))
            .ok_or(Error::Overflow("v2 directory entry count"))?;
        let directory_end = HEADER_SIZE
            .checked_add(
                directory_entry_count
                    .checked_mul(GLOBAL_DATA_SIZE)
                    .ok_or(Error::Overflow("v2 directory size"))?,
            )
            .ok_or(Error::Overflow("v2 directory end"))?;
        let mut current = directory_end;

        let uuid = if has_uuid {
            let end = current
                .checked_add(uuid_size)
                .ok_or(Error::Overflow("v2 UUID section end"))?;
            let range = current..end;
            current = end;
            Some(range)
        } else {
            None
        };

        current = align_up(current, usize::from(self.address_offset_size))
            .ok_or(Error::Overflow("v2 address-table alignment"))?;
        let address_size = usize::try_from(self.address_count)
            .map_err(|_| Error::Overflow("v2 address count"))?
            .checked_mul(usize::from(self.address_offset_size))
            .ok_or(Error::Overflow("v2 address-table size"))?;
        let address_offsets = checked_range(current, address_size, "v2 address table")?;
        current = address_offsets.end;

        current = align_up(current, usize::from(ADDRESS_INFO_OFFSET_SIZE))
            .ok_or(Error::Overflow("v2 address-info alignment"))?;
        let address_info_size = usize::try_from(self.address_count)
            .map_err(|_| Error::Overflow("v2 address count"))?
            .checked_mul(usize::from(ADDRESS_INFO_OFFSET_SIZE))
            .ok_or(Error::Overflow("v2 address-info size"))?;
        let address_info_offsets =
            checked_range(current, address_info_size, "v2 address-info table")?;
        current = address_info_offsets.end;

        current = align_up(current, 4).ok_or(Error::Overflow("v2 file-table alignment"))?;
        let file_size = file_table_size(file_count)?;
        let file_table = checked_range(current, file_size, "v2 file table")?;
        current = file_table.end;

        let string_table = checked_range(current, string_table_size, "v2 string table")?;
        current = string_table.end;

        current = align_up(current, 4).ok_or(Error::Overflow("v2 FunctionInfo alignment"))?;
        let function_info = checked_range(current, function_info_size, "v2 FunctionInfo section")?;
        current = function_info.end;

        let mut directory = Vec::with_capacity(directory_entry_count.saturating_sub(1));
        if let Some(range) = &uuid {
            directory.push(global_from_range(GLOBAL_UUID, range)?);
        }
        directory.push(global_from_range(GLOBAL_ADDRESS_OFFSETS, &address_offsets)?);
        directory.push(global_from_range(
            GLOBAL_ADDRESS_INFO_OFFSETS,
            &address_info_offsets,
        )?);
        directory.push(global_from_range(GLOBAL_FILE_TABLE, &file_table)?);
        directory.push(global_from_range(GLOBAL_STRING_TABLE, &string_table)?);
        directory.push(global_from_range(GLOBAL_FUNCTION_INFO, &function_info)?);

        Ok(WriteLayout {
            directory: directory.into_boxed_slice(),
            directory_end,
            uuid,
            address_offsets,
            address_info_offsets,
            file_table,
            string_table,
            function_info,
            file_size: current,
        })
    }
}

fn validate_address_offset_size(size: u8) -> Result<()> {
    if (1..=8).contains(&size) {
        Ok(())
    } else {
        Err(Error::InvalidAddressOffsetSize {
            version: VERSION,
            size,
        })
    }
}

impl GlobalData {
    pub(crate) fn decode_from(cursor: &mut Cursor<'_>) -> Result<Self> {
        Ok(Self {
            section_type: cursor.read_u32()?,
            file_offset: cursor.read_u64()?,
            file_size: cursor.read_u64()?,
        })
    }

    pub(crate) fn encode_into(self, output: &mut Encoder) {
        output.write_u32(self.section_type);
        output.write_u64(self.file_offset);
        output.write_u64(self.file_size);
    }
}

pub(crate) fn parse_directory(bytes: &[u8], endian: Endian, header: &Header) -> Result<Directory> {
    let mut cursor = Cursor::at(bytes, endian, HEADER_SIZE)?;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    loop {
        let entry = GlobalData::decode_from(&mut cursor)?;
        if entry.section_type == GLOBAL_END {
            if entry.file_offset != 0 || entry.file_size != 0 {
                return Err(Error::InvalidFormat(
                    "v2 directory terminator is not all zero",
                ));
            }
            break;
        }
        if !seen.insert(entry.section_type) {
            return Err(Error::DuplicateSection(entry.section_type));
        }
        if entry.file_size == 0 {
            return Err(Error::ZeroSizedSection {
                section_type: entry.section_type,
            });
        }
        let end = entry
            .file_offset
            .checked_add(entry.file_size)
            .ok_or(Error::Overflow("v2 section end"))?;
        if end > bytes.len() as u64 {
            return Err(Error::SectionOutOfBounds {
                section_type: entry.section_type,
                offset: entry.file_offset,
                size: entry.file_size,
                input_len: bytes.len(),
            });
        }
        entries.push(entry);
    }
    let end_offset = cursor.position();
    for required in REQUIRED_TYPES {
        if !seen.contains(&required) {
            return Err(Error::MissingSection(required));
        }
    }

    validate_sections(bytes, end_offset, header, &entries)?;
    Ok(Directory {
        entries: entries.into_boxed_slice(),
        end_offset,
    })
}

fn validate_sections(
    bytes: &[u8],
    directory_end: usize,
    header: &Header,
    entries: &[GlobalData],
) -> Result<()> {
    let address_size = u64::from(header.address_count)
        .checked_mul(u64::from(header.address_offset_size))
        .ok_or(Error::Overflow("v2 address-table size"))?;
    let address_info_size = u64::from(header.address_count)
        .checked_mul(u64::from(ADDRESS_INFO_OFFSET_SIZE))
        .ok_or(Error::Overflow("v2 address-info size"))?;
    for entry in entries {
        match entry.section_type {
            GLOBAL_ADDRESS_OFFSETS if entry.file_size != address_size => {
                return Err(Error::InvalidFormat("v2 address-table size mismatch"));
            }
            GLOBAL_ADDRESS_OFFSETS
                if entry
                    .file_offset
                    .checked_rem(u64::from(header.address_offset_size))
                    .is_none_or(|remainder| remainder != 0) =>
            {
                return Err(Error::InvalidFormat("v2 address table is misaligned"));
            }
            GLOBAL_ADDRESS_INFO_OFFSETS if entry.file_size != address_info_size => {
                return Err(Error::InvalidFormat("v2 address-info table size mismatch"));
            }
            GLOBAL_ADDRESS_INFO_OFFSETS
                if entry
                    .file_offset
                    .checked_rem(u64::from(ADDRESS_INFO_OFFSET_SIZE))
                    .is_none_or(|remainder| remainder != 0) =>
            {
                return Err(Error::InvalidFormat("v2 address-info table is misaligned"));
            }
            GLOBAL_FILE_TABLE
                if entry.file_size < 4
                    || entry
                        .file_size
                        .saturating_sub(4)
                        .checked_rem(FILE_ENTRY_SIZE as u64)
                        .is_none_or(|remainder| remainder != 0) =>
            {
                return Err(Error::InvalidFormat("invalid v2 file-table size"));
            }
            GLOBAL_FILE_TABLE if entry.file_offset % 4 != 0 => {
                return Err(Error::InvalidFormat("v2 file table is misaligned"));
            }
            GLOBAL_STRING_TABLE => {
                let offset = usize::try_from(entry.file_offset)
                    .map_err(|_| Error::Overflow("v2 string-table offset"))?;
                if bytes.get(offset).copied() != Some(0) {
                    return Err(Error::InvalidFormat(
                        "GSYM string table does not begin with an empty string",
                    ));
                }
            }
            GLOBAL_FUNCTION_INFO if entry.file_offset % 4 != 0 => {
                return Err(Error::InvalidFormat(
                    "v2 FunctionInfo section is misaligned",
                ));
            }
            _ => {}
        }
    }

    let mut ranges: Vec<(u64, u64, u32)> = entries
        .iter()
        .map(|entry| {
            Ok((
                entry.file_offset,
                entry
                    .file_offset
                    .checked_add(entry.file_size)
                    .ok_or(Error::Overflow("v2 section end"))?,
                entry.section_type,
            ))
        })
        .collect::<Result<_>>()?;
    ranges.sort_unstable_by_key(|range| range.0);
    let mut previous_end = directory_end as u64;
    for (start, end, _) in ranges {
        if start < previous_end {
            return Err(Error::InvalidFormat(
                "v2 sections overlap each other or the directory",
            ));
        }
        previous_end = end;
    }
    Ok(())
}

pub(crate) fn file_table_size(file_count: u32) -> Result<usize> {
    super::file_table_size(file_count, FILE_ENTRY_SIZE, "v2 file-table size")
}

fn checked_range(start: usize, size: usize, context: &'static str) -> Result<Range<usize>> {
    let end = start.checked_add(size).ok_or(Error::Overflow(context))?;
    Ok(start..end)
}

fn global_from_range(section_type: u32, range: &Range<usize>) -> Result<GlobalData> {
    Ok(GlobalData {
        section_type,
        file_offset: u64::try_from(range.start).map_err(|_| Error::Overflow("file offset"))?,
        file_size: u64::try_from(range.len()).map_err(|_| Error::Overflow("file size"))?,
    })
}

#[cfg(test)]
mod tests {
    use crate::endian::{Encoder, Endian};

    use super::{
        GLOBAL_END, GlobalData, HEADER_SIZE, Header, STRING_TABLE_ENCODING_DEFAULT, parse_directory,
    };

    #[test]
    fn header_round_trip() {
        let header = Header {
            address_offset_size: 4,
            string_table_encoding: STRING_TABLE_ENCODING_DEFAULT,
            base_address: 0x1000,
            address_count: 3,
        };
        for endian in [Endian::Little, Endian::Big] {
            let bytes = header.encode(endian).unwrap();
            assert_eq!(bytes.len(), HEADER_SIZE);
            assert_eq!(Header::decode(&bytes).unwrap(), (header.clone(), endian));
        }
    }

    #[test]
    fn header_accepts_all_v2_address_widths() {
        for address_offset_size in 1..=8 {
            let header = Header {
                address_offset_size,
                string_table_encoding: STRING_TABLE_ENCODING_DEFAULT,
                base_address: 0x1000,
                address_count: 3,
            };
            let bytes = header.encode(Endian::Little).unwrap();
            assert_eq!(Header::decode(&bytes).unwrap().0, header);
        }
    }

    #[test]
    fn writer_layout_has_required_alignment_and_directory_order() {
        let header = Header {
            address_offset_size: 8,
            string_table_encoding: 0,
            base_address: 0,
            address_count: 3,
        };
        let layout = header.write_layout(20, 2, 17, 91).unwrap();
        assert_eq!(layout.directory_end, 160);
        assert_eq!(layout.uuid, Some(160..180));
        assert_eq!(layout.address_offsets.start % 8, 0);
        assert_eq!(layout.address_info_offsets.start % 8, 0);
        assert_eq!(layout.file_table.start % 4, 0);
        assert_eq!(layout.function_info.start % 4, 0);
        assert_eq!(layout.directory.len(), 6);
    }

    #[test]
    fn parses_checked_directory() {
        let header = Header {
            address_offset_size: 1,
            string_table_encoding: 0,
            base_address: 0x1000,
            address_count: 1,
        };
        let layout = header.write_layout(0, 1, 1, 16).unwrap();
        let mut output = Encoder::new(Endian::Little);
        output.write_bytes(&header.encode(Endian::Little).unwrap());
        for entry in &layout.directory {
            entry.encode_into(&mut output);
        }
        GlobalData {
            section_type: GLOBAL_END,
            file_offset: 0,
            file_size: 0,
        }
        .encode_into(&mut output);
        output.as_slice();
        let mut bytes = output.into_inner();
        bytes.resize(layout.file_size, 0);
        assert_eq!(
            parse_directory(&bytes, Endian::Little, &header)
                .unwrap()
                .entries
                .len(),
            5
        );
    }
}

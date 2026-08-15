use std::ops::Range;

use crate::endian::{Cursor, Endian};
use crate::error::{Error, Result};
use crate::format::{v1, v2};

#[derive(Clone, Copy, Debug)]
pub(super) enum VersionLayout {
    V1,
    V2,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedLayout {
    pub(super) version: VersionLayout,
    pub(super) endian: Endian,
    pub(super) address_offset_size: u8,
    pub(super) string_offset_size: u8,
    pub(super) base_address: u64,
    pub(super) address_count: u32,
    pub(super) build_id: Range<usize>,
    pub(super) address_offsets: Range<usize>,
    pub(super) address_info_offsets: Range<usize>,
    pub(super) file_table: Range<usize>,
    pub(super) string_table: Range<usize>,
    pub(super) function_info: Range<usize>,
    pub(super) file_count: u32,
}

pub(super) fn parse(data: &[u8]) -> Result<ParsedLayout> {
    let endian = v1::detect_endian(data)?;
    let mut prefix = Cursor::new(data, endian);
    let _magic = prefix.read_u32()?;
    match prefix.read_u16()? {
        v1::VERSION => parse_v1(data),
        v2::VERSION => parse_v2(data),
        other => Err(Error::UnsupportedVersion(other)),
    }
}

fn parse_v1(data: &[u8]) -> Result<ParsedLayout> {
    let (header, endian) = v1::Header::decode(data)?;
    let layout = header.layout(data.len())?;
    let mut files = Cursor::at(data, endian, layout.file_table_offset)?;
    let file_count = files.read_u32()?;
    let file_size = v1::file_table_size(file_count)?;
    let file_end = layout
        .file_table_offset
        .checked_add(file_size)
        .ok_or(Error::Overflow("v1 file table end"))?;
    if file_end > layout.string_table.start {
        return Err(Error::InvalidFormat("v1 file table overlaps string table"));
    }
    let uuid_size = header.uuid.as_slice().len();
    Ok(ParsedLayout {
        version: VersionLayout::V1,
        endian,
        address_offset_size: header.address_offset_size,
        string_offset_size: v1::STRING_OFFSET_SIZE,
        base_address: header.base_address,
        address_count: header.address_count,
        build_id: 28..28_usize.saturating_add(uuid_size),
        address_offsets: layout.address_offsets,
        address_info_offsets: layout.address_info_offsets,
        file_table: layout.file_table_offset..file_end,
        string_table: layout.string_table,
        function_info: layout.function_info_offset..data.len(),
        file_count,
    })
}

fn parse_v2(data: &[u8]) -> Result<ParsedLayout> {
    let (header, endian) = v2::Header::decode(data)?;
    let directory = v2::parse_directory(data, endian, &header)?;
    let range = |kind| -> Result<Range<usize>> {
        let entry = directory.get(kind).ok_or(Error::MissingSection(kind))?;
        let start =
            usize::try_from(entry.file_offset).map_err(|_| Error::Overflow("section offset"))?;
        let size = usize::try_from(entry.file_size).map_err(|_| Error::Overflow("section size"))?;
        Ok(start
            ..start
                .checked_add(size)
                .ok_or(Error::Overflow("section end"))?)
    };
    let file_table = range(v2::GLOBAL_FILE_TABLE)?;
    let mut file_cursor = Cursor::at(data, endian, file_table.start)?;
    let file_count = file_cursor.read_u32()?;
    if v2::file_table_size(file_count)? != file_table.len() {
        return Err(Error::InvalidFormat(
            "v2 file-table count does not match its size",
        ));
    }
    let build_id = directory
        .get(v2::GLOBAL_UUID)
        .map(|_| range(v2::GLOBAL_UUID))
        .transpose()?
        .unwrap_or(0..0);
    Ok(ParsedLayout {
        version: VersionLayout::V2,
        endian,
        address_offset_size: header.address_offset_size,
        string_offset_size: v2::STRING_OFFSET_SIZE,
        base_address: header.base_address,
        address_count: header.address_count,
        build_id,
        address_offsets: range(v2::GLOBAL_ADDRESS_OFFSETS)?,
        address_info_offsets: range(v2::GLOBAL_ADDRESS_INFO_OFFSETS)?,
        file_table,
        string_table: range(v2::GLOBAL_STRING_TABLE)?,
        function_info: range(v2::GLOBAL_FUNCTION_INFO)?,
        file_count,
    })
}

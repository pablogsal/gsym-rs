use crate::endian::Encoder;
use crate::format::{self, align_up};
use crate::model::Function;
use crate::{Endian, Error, Result};

use super::WriterOptions;
use super::semantic::{StringTable, encode_function};

pub(super) struct EncodedImage {
    pub(super) prefix: Vec<u8>,
    pub(super) function_info: Vec<u8>,
}

impl EncodedImage {
    pub(super) fn into_bytes(mut self) -> Vec<u8> {
        self.function_info.reserve_exact(self.prefix.len());
        self.function_info.splice(0..0, self.prefix);
        self.function_info
    }
}

const AVERAGE_FUNCTION_LEN: usize = 24;

pub(super) fn address_offset_width(maximum: u64) -> u8 {
    if u8::try_from(maximum).is_ok() {
        1
    } else if u16::try_from(maximum).is_ok() {
        2
    } else if u32::try_from(maximum).is_ok() {
        4
    } else {
        8
    }
}

pub(super) fn encode_v1(
    options: &WriterOptions,
    base: u64,
    width: u8,
    files: &[(u64, u64)],
    strings: &[u8],
    functions: FunctionSection,
) -> Result<EncodedImage> {
    if options.build_id.len() > format::v1::MAX_UUID_SIZE {
        return Err(Error::V1BuildIdTooLong {
            size: options.build_id.len(),
        });
    }
    let count = function_count(&functions)?;
    let address_end = format::v1::HEADER_SIZE
        .checked_add(checked_table_size(
            functions.addresses.len(),
            usize::from(width),
            "v1 address",
        )?)
        .ok_or(Error::Overflow("v1 address table end"))?;
    let info_start = align_up(address_end, 4).ok_or(Error::Overflow("v1 info alignment"))?;
    let info_end = info_start
        .checked_add(checked_table_size(
            functions.addresses.len(),
            4,
            "v1 address-info",
        )?)
        .ok_or(Error::Overflow("v1 address-info table end"))?;
    let file_start = align_up(info_end, 4).ok_or(Error::Overflow("v1 file alignment"))?;
    let file_size = 4_usize
        .checked_add(checked_table_size(files.len(), 8, "v1 file")?)
        .ok_or(Error::Overflow("v1 file table size"))?;
    let string_start = file_start
        .checked_add(file_size)
        .ok_or(Error::Overflow("v1 string offset"))?;
    let string_end = string_start
        .checked_add(strings.len())
        .ok_or(Error::Overflow("v1 FunctionInfo base"))?;
    let function_base = align_up(string_end, 4).ok_or(Error::Overflow("v1 FunctionInfo base"))?;
    let header = format::v1::Header {
        address_offset_size: width,
        base_address: base,
        address_count: count,
        string_table_offset: narrow_v1(string_start, "string-table offset")?,
        string_table_size: narrow_v1(strings.len(), "string-table size")?,
        uuid: format::v1::FixedUuid::new(&options.build_id)?,
    };

    let mut output = Encoder::with_capacity(options.endian, function_base);
    output.write_bytes(&header.encode(options.endian)?);
    write_address_offsets(&mut output, &functions, base, width)?;
    output.align_to(4)?;
    for offset in &functions.offsets {
        let absolute = function_base
            .checked_add(*offset)
            .ok_or(Error::Overflow("FunctionInfo offset"))?;
        output.write_u32(narrow_v1(absolute, "FunctionInfo offset")?);
    }
    output.align_to(4)?;
    write_file_table(&mut output, files, 4)?;
    debug_assert_eq!(output.len(), string_start);
    output.write_bytes(strings);
    output.align_to(4)?;
    debug_assert_eq!(output.len(), function_base);
    Ok(EncodedImage {
        prefix: output.into_inner(),
        function_info: functions.bytes,
    })
}

pub(super) fn encode_v2(
    options: &WriterOptions,
    base: u64,
    width: u8,
    files: &[(u64, u64)],
    strings: &[u8],
    functions: FunctionSection,
) -> Result<EncodedImage> {
    let header = format::v2::Header {
        address_offset_size: width,
        string_table_encoding: format::v2::STRING_TABLE_ENCODING_DEFAULT,
        base_address: base,
        address_count: function_count(&functions)?,
    };
    let layout = header.write_layout(
        options.build_id.len(),
        u32::try_from(files.len()).map_err(|_| Error::Limit {
            context: "file count",
            value: files.len() as u64,
            limit: u64::from(u32::MAX),
        })?,
        strings.len(),
        functions.bytes.len(),
    )?;

    let mut output = Encoder::with_capacity(options.endian, layout.function_info.start);
    output.write_bytes(&header.encode(options.endian)?);
    for entry in &layout.directory {
        entry.encode_into(&mut output);
    }
    format::v2::GlobalData {
        section_type: format::v2::GLOBAL_END,
        file_offset: 0,
        file_size: 0,
    }
    .encode_into(&mut output);
    debug_assert_eq!(output.len(), layout.directory_end);
    if let Some(uuid) = &layout.uuid {
        debug_assert_eq!(output.len(), uuid.start);
        output.write_bytes(&options.build_id);
    }
    output.align_to(usize::from(width))?;
    debug_assert_eq!(output.len(), layout.address_offsets.start);
    write_address_offsets(&mut output, &functions, base, width)?;
    output.align_to(8)?;
    debug_assert_eq!(output.len(), layout.address_info_offsets.start);
    for offset in &functions.offsets {
        output.write_u64(*offset as u64);
    }
    output.align_to(4)?;
    debug_assert_eq!(output.len(), layout.file_table.start);
    write_file_table(&mut output, files, 8)?;
    debug_assert_eq!(output.len(), layout.string_table.start);
    output.write_bytes(strings);
    output.align_to(4)?;
    debug_assert_eq!(output.len(), layout.function_info.start);
    debug_assert_eq!(
        output.len().saturating_add(functions.bytes.len()),
        layout.file_size
    );
    Ok(EncodedImage {
        prefix: output.into_inner(),
        function_info: functions.bytes,
    })
}

fn function_count(functions: &FunctionSection) -> Result<u32> {
    u32::try_from(functions.addresses.len()).map_err(|_| Error::Limit {
        context: "function count",
        value: functions.addresses.len() as u64,
        limit: u64::from(u32::MAX),
    })
}

fn checked_table_size(count: usize, width: usize, context: &'static str) -> Result<usize> {
    count.checked_mul(width).ok_or(Error::Overflow(context))
}

fn narrow_v1(value: usize, field: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::V1LimitExceeded {
        field,
        value: value as u64,
    })
}

pub(super) struct FunctionSection {
    addresses: Vec<u64>,
    offsets: Vec<usize>,
    pub(super) bytes: Vec<u8>,
}

pub(super) fn encode_functions(
    functions: Vec<Function>,
    strings: &mut StringTable,
    endian: Endian,
    string_width: u8,
) -> Result<FunctionSection> {
    let mut output =
        Encoder::with_capacity(endian, functions.len().saturating_mul(AVERAGE_FUNCTION_LEN));
    let mut addresses = Vec::with_capacity(functions.len());
    let mut offsets = Vec::with_capacity(functions.len());
    for function in functions {
        let function = encode_function(function, strings);
        output.align_to(4)?;
        addresses.push(function.range.start);
        offsets.push(output.len());
        format::function::encode_into(&function, &mut output, string_width, false)?;
    }
    Ok(FunctionSection {
        addresses,
        offsets,
        bytes: output.into_inner(),
    })
}

fn write_address_offsets(
    output: &mut Encoder,
    functions: &FunctionSection,
    base: u64,
    width: u8,
) -> Result<()> {
    output.align_to(usize::from(width))?;
    for address in &functions.addresses {
        output.write_uint(
            address.checked_sub(base).ok_or(Error::InvalidModel(
                "function address precedes the image base",
            ))?,
            width,
        )?;
    }
    Ok(())
}

fn write_file_table(output: &mut Encoder, files: &[(u64, u64)], string_width: u8) -> Result<()> {
    output.write_u32(u32::try_from(files.len()).map_err(|_| Error::Limit {
        context: "file count",
        value: files.len() as u64,
        limit: u64::from(u32::MAX),
    })?);
    for (directory, basename) in files {
        output.write_uint(*directory, string_width)?;
        output.write_uint(*basename, string_width)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{AddressRange, Function, Gsym, GsymBuilder, GsymVersion};

    #[test]
    fn function_records_align_after_each_string_table_length() {
        for version in [GsymVersion::V1, GsymVersion::V2] {
            for length in 1..=8 {
                let mut builder = GsymBuilder::new().version(version);
                builder
                    .add_function(Function::new(
                        AddressRange::new(0x1000, 0x1010),
                        vec![b'a'; length],
                    ))
                    .unwrap();
                builder
                    .add_function(Function::new(AddressRange::new(0x1020, 0x1030), b"second"))
                    .unwrap();
                let bytes = builder.to_bytes().unwrap();
                let gsym = Gsym::parse(&bytes).unwrap();
                gsym.verify().unwrap();
                assert_eq!(
                    gsym.lookup(0x1000)
                        .unwrap()
                        .unwrap()
                        .frames()
                        .first()
                        .unwrap()
                        .name,
                    vec![b'a'; length]
                );
                assert_eq!(
                    gsym.lookup(0x1020)
                        .unwrap()
                        .unwrap()
                        .frames()
                        .first()
                        .unwrap()
                        .name,
                    b"second"
                );
            }
        }
    }
}

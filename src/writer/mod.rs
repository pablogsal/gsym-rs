mod encode;
mod finalize;
mod semantic;

use std::fmt;
use std::io::Write;

use self::encode::{address_offset_width, encode_functions, encode_v1, encode_v2};
use self::finalize::finalize;
use self::semantic::{StringTable, intern_files, validate_merged_depth};
use crate::builder::GsymBuilder;
use crate::validation::validate_file_table;
use crate::{Endian, Error, GsymVersion, Result};

/// Settings controlling deterministic GSYM encoding.
///
/// Version, byte order, base address, and build ID.
/// [`BuilderOptions`](crate::BuilderOptions) holds these alongside the
/// model-level policy, and the [`GsymBuilder`] setters change the same values.
///
/// A `None` base address selects the lowest function address. `build_id` is
/// opaque and is stored as given; version 1 rejects more than 20 bytes with
/// [`Error::V1BuildIdTooLong`](crate::Error::V1BuildIdTooLong).
#[derive(Clone, Eq, PartialEq)]
pub struct WriterOptions {
    /// Output format version.
    pub version: GsymVersion,
    /// Output byte order.
    pub endian: Endian,
    /// Explicit image base, or `None` to use the first function address.
    pub base_address: Option<u64>,
    /// Opaque build identifier copied into the output header.
    pub build_id: Vec<u8>,
}

impl fmt::Debug for WriterOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WriterOptions")
            .field("version", &self.version)
            .field("endian", &self.endian)
            .field("base_address", &self.base_address)
            .field("build_id_len", &self.build_id.len())
            .finish()
    }
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            version: GsymVersion::V1,
            endian: Endian::Little,
            base_address: None,
            build_id: Vec::new(),
        }
    }
}

pub(crate) fn write_builder(builder: GsymBuilder, mut output: impl Write) -> Result<()> {
    let image = encode_builder(builder)?;
    output.write_all(&image.prefix)?;
    output.write_all(&image.function_info)?;
    Ok(())
}

fn encode_builder(builder: GsymBuilder) -> Result<encode::EncodedImage> {
    validate_file_table(builder.files())?;
    builder.validate_file_references()?;
    let (options, function_set, files, functions) = builder.into_parts();
    let functions = finalize(functions, &options, function_set);
    let first = functions
        .first()
        .ok_or(Error::InvalidModel("at least one function is required"))?;
    let base = options.writer.base_address.unwrap_or(first.range.start);
    if functions.iter().any(|function| function.range.start < base) {
        return Err(Error::InvalidModel(
            "function address precedes the selected image base",
        ));
    }
    let width = address_offset_width(
        functions
            .last()
            .ok_or(Error::InvalidModel("at least one function is required"))?
            .range
            .start
            .saturating_sub(base),
    );

    // Finalization can add a parent around an existing merged tree. Check the
    // resulting depth before checking format-specific limits.
    validate_merged_depth(&functions, 0)?;
    let mut strings = StringTable::with_capacity(
        functions
            .len()
            .saturating_add(files.len().saturating_mul(2)),
    );
    let encoded_files = intern_files(&files, &mut strings);
    if options.writer.version == GsymVersion::V1
        && options.writer.build_id.len() > crate::format::v1::MAX_UUID_SIZE
    {
        return Err(Error::V1BuildIdTooLong {
            size: options.writer.build_id.len(),
        });
    }
    let string_width = match options.writer.version {
        GsymVersion::V1 => 4,
        GsymVersion::V2 => 8,
    };
    let encoded_functions =
        encode_functions(functions, &mut strings, options.writer.endian, string_width)?;
    let image = match options.writer.version {
        GsymVersion::V1 => encode_v1(
            &options.writer,
            base,
            width,
            &encoded_files,
            strings.bytes(),
            encoded_functions,
        )?,
        GsymVersion::V2 => encode_v2(
            &options.writer,
            base,
            width,
            &encoded_files,
            strings.bytes(),
            encoded_functions,
        )?,
    };
    Ok(image)
}

pub(crate) fn encode_builder_to_bytes(builder: GsymBuilder) -> Result<Vec<u8>> {
    encode_builder(builder).map(encode::EncodedImage::into_bytes)
}

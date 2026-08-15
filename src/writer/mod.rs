mod encode;
mod finalize;
mod semantic;

use std::fmt;
use std::io::Write;

use self::encode::{address_offset_width, encode_v1, encode_v2};
use self::finalize::finalize;
use self::semantic::{StringTable, encode_function, intern_files};
use crate::builder::GsymBuilder;
use crate::validation::{validate_file_table, validate_for_writer};
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
    let (options, function_set, files, functions) = builder.into_parts();
    validate_file_table(&files)?;
    for function in &functions {
        validate_for_writer(function, files.len())?;
    }
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

    let mut strings = StringTable::with_capacity(
        functions
            .len()
            .saturating_add(files.len().saturating_mul(2)),
    );
    let encoded_files = intern_files(&files, &mut strings);
    let mut encoded_functions = Vec::with_capacity(functions.len());
    for function in functions {
        encoded_functions.push(encode_function(function, &mut strings)?);
    }
    let image = match options.writer.version {
        GsymVersion::V1 => encode_v1(
            &options.writer,
            base,
            width,
            &encoded_files,
            strings.bytes(),
            &encoded_functions,
        )?,
        GsymVersion::V2 => encode_v2(
            &options.writer,
            base,
            width,
            &encoded_files,
            strings.bytes(),
            &encoded_functions,
        )?,
    };
    Ok(image)
}

pub(crate) fn encode_builder_to_bytes(builder: GsymBuilder) -> Result<Vec<u8>> {
    encode_builder(builder).map(encode::EncodedImage::into_bytes)
}

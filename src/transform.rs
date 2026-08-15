use std::fmt;

use crate::{
    Endian, Error, FileEntry, FileIndex, Function, Gsym, GsymBuilder, GsymVersion, InlineNode,
    Result,
};

impl<D: AsRef<[u8]>> Gsym<D> {
    /// Decodes every file and function into an owned semantic model.
    ///
    /// A record type this crate cannot represent is an error rather than a
    /// silent drop.
    ///
    /// This decodes and validates the whole file, so it costs about as much as
    /// [`Self::verify`].
    ///
    /// # Errors
    ///
    /// Returns the first structural, reference, or semantic decoding error.
    pub fn decode_all(&self) -> Result<DecodedGsym> {
        let (report, functions) = self.decode_all_verified()?;
        let header = self.header();
        let mut files = Vec::with_capacity(report.files.max(1));
        if report.files == 0 {
            files.push(FileEntry::default());
        }
        for index in 0..report.files {
            let index = u32::try_from(index).map_err(|_| Error::Overflow("file index"))?;
            let (directory, basename) = self.file(index)?;
            files.push(FileEntry {
                directory: directory.to_vec(),
                basename: basename.to_vec(),
            });
        }
        Ok(DecodedGsym {
            source_version: header.version,
            source_endian: header.endian,
            base_address: header.base_address,
            build_id: header.build_id.to_vec(),
            files,
            functions,
        })
    }

    /// Re-encodes this file with a selected version or byte order.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is malformed or the semantic data cannot
    /// be represented by the requested output version.
    pub fn transcode(&self, options: TranscodeOptions) -> Result<Vec<u8>> {
        self.decode_all()?.transcode(options)
    }
}

/// An owned, version-independent representation of a complete GSYM file.
///
/// Decode with [`Gsym::decode_all`](crate::Gsym::decode_all), edit the public
/// semantic fields when needed, then use [`Self::into_builder`] or
/// [`Self::transcode`] to move the model into a new encoding.
///
/// Decoding is all-or-nothing: a record type this crate cannot represent is an
/// error rather than a silent drop. `source_version` and `source_endian` record
/// what the input used, and [`TranscodeOptions`] overrides either one for the
/// output.
///
/// File indices in the model refer to [`Self::files`], including the reserved
/// empty entry at index zero. Re-encoding renumbers the table and keeps only
/// the files the retained functions reference.
#[derive(Eq, PartialEq)]
pub struct DecodedGsym {
    /// Version from which this model was decoded.
    pub source_version: GsymVersion,
    /// Byte order from which this model was decoded.
    pub source_endian: Endian,
    /// Image base address.
    pub base_address: u64,
    /// Opaque build identifier.
    pub build_id: Vec<u8>,
    /// Complete file table, including reserved index zero.
    pub files: Vec<FileEntry>,
    /// Fully decoded semantic functions.
    pub functions: Vec<Function>,
}

impl fmt::Debug for DecodedGsym {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedGsym")
            .field("source_version", &self.source_version)
            .field("source_endian", &self.source_endian)
            .field("base_address", &self.base_address)
            .field("build_id_len", &self.build_id.len())
            .field("file_count", &self.files.len())
            .field("function_count", &self.functions.len())
            .finish_non_exhaustive()
    }
}

/// Output choices for semantic GSYM transcoding.
///
/// `Default` keeps both the version and the byte order of the input, which
/// makes a transcode a pure re-encode. Setting a field converts that property.
///
/// ```
/// use gsym::{Endian, GsymVersion, TranscodeOptions};
///
/// let keep_everything = TranscodeOptions::default();
/// assert!(keep_everything.version.is_none());
///
/// let to_big_endian_v2 = TranscodeOptions {
///     version: Some(GsymVersion::V2),
///     endian: Some(Endian::Big),
/// };
/// # let _ = to_big_endian_v2;
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TranscodeOptions {
    /// Preserve the input version when absent.
    pub version: Option<GsymVersion>,
    /// Preserve the input byte order when absent.
    pub endian: Option<Endian>,
}

/// One independently readable shard of a segmented GSYM image.
///
/// Each segment is a complete GSYM file that [`Gsym::parse`](crate::Gsym::parse)
/// accepts on its own, holding a contiguous span of the source file's functions
/// and only the source files those functions reference. The address fields let
/// a consumer pick the right shard for an address without opening it.
///
/// Produced by [`DecodedGsym::segments`].
#[derive(Eq, PartialEq)]
pub struct GsymSegment {
    /// Lowest function start address in this segment.
    pub first_address: u64,
    /// Exclusive maximum function end address in this segment.
    pub end_address: u64,
    /// Number of top-level functions in this segment.
    pub function_count: usize,
    /// Complete independently readable GSYM image.
    pub bytes: Box<[u8]>,
}

impl fmt::Debug for GsymSegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GsymSegment")
            .field("first_address", &self.first_address)
            .field("end_address", &self.end_address)
            .field("function_count", &self.function_count)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

impl DecodedGsym {
    /// Builds a deterministic writer model by cloning this decoded model.
    ///
    /// Use [`Self::into_builder`] when this model is no longer needed.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid file references or model data.
    pub fn to_builder(&self, options: TranscodeOptions) -> Result<GsymBuilder> {
        self.builder_for_functions(self.functions.iter(), options)
    }

    /// Converts this decoded model into a deterministic writer without cloning
    /// its file or function trees.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid file references or model data.
    pub fn into_builder(self, options: TranscodeOptions) -> Result<GsymBuilder> {
        let Self {
            source_version,
            source_endian,
            base_address,
            build_id,
            files,
            functions,
        } = self;
        if files
            .first()
            .is_none_or(|file| *file != FileEntry::default())
        {
            return Err(Error::InvalidModel("file-table index zero must be empty"));
        }
        let mut used = vec![false; files.len()];
        if let Some(zero) = used.first_mut() {
            *zero = true;
        }
        for function in &functions {
            mark_function_files(function, &mut used)?;
        }
        let mut builder = GsymBuilder::new()
            .version(options.version.unwrap_or(source_version))
            .endian(options.endian.unwrap_or(source_endian))
            .base_address(base_address)
            .build_id(build_id)
            .repair_zero_sized_functions(false);
        let mut remap = vec![FileIndex::ZERO; files.len()];
        for (old, file) in files.into_iter().enumerate().skip(1) {
            if used.get(old).copied().unwrap_or(false)
                && let Some(slot) = remap.get_mut(old)
            {
                *slot = builder.add_file(file)?;
            }
        }
        for mut function in functions {
            remap_function_files(&mut function, &remap)?;
            builder.add_function(function)?;
        }
        Ok(builder)
    }

    /// Encodes the complete model using the requested output settings.
    ///
    /// ```
    /// use gsym::{
    ///     AddressRange, Endian, Function, Gsym, GsymBuilder, GsymVersion,
    ///     TranscodeOptions,
    /// };
    ///
    /// let mut builder = GsymBuilder::new();
    /// builder.add_function(Function::new(
    ///     AddressRange::new(0x4000, 0x4010),
    ///     b"transcoded",
    /// ))?;
    /// let source = builder.to_bytes()?;
    /// let decoded = Gsym::parse(source)?.decode_all()?;
    /// let output = decoded.transcode(TranscodeOptions {
    ///     version: Some(GsymVersion::V2),
    ///     endian: Some(Endian::Big),
    /// })?;
    ///
    /// let reparsed = Gsym::parse(output)?;
    /// assert_eq!(reparsed.header().version, GsymVersion::V2);
    /// assert_eq!(reparsed.header().endian, Endian::Big);
    /// # Ok::<(), gsym::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when the model cannot be represented by the requested
    /// format version.
    pub fn transcode(self, options: TranscodeOptions) -> Result<Vec<u8>> {
        self.into_builder(options)?.to_bytes()
    }

    /// Splits the model into independently valid GSYM files near `target_size`.
    ///
    /// A single record larger than the target is emitted alone. Each shard
    /// contains only source files referenced by its functions. Boundaries are
    /// selected from exact encoded sizes, so all multi-function shards are at
    /// most the requested target.
    ///
    /// Each shard covers a contiguous address span, so
    /// [`GsymSegment::first_address`] and [`GsymSegment::end_address`] are
    /// enough to route an address to a shard. Segmentation is considerably more
    /// expensive than a single encode.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero target, empty model, invalid references, or
    /// an encoding failure.
    pub fn segments(
        &self,
        target_size: usize,
        options: TranscodeOptions,
    ) -> Result<Vec<GsymSegment>> {
        if target_size == 0 {
            return Err(Error::InvalidModel("segment target size must not be zero"));
        }
        if self.functions.is_empty() {
            return Err(Error::InvalidModel("at least one function is required"));
        }
        let mut functions = self.functions.iter().collect::<Vec<_>>();
        functions.sort_by_key(|function| (function.range.start, function.range.end));
        let mut segments = Vec::new();
        let mut start = 0;
        while start < functions.len() {
            let minimum = start.saturating_add(1);
            let mut low = minimum;
            let mut high = functions.len();
            let mut best = minimum;
            let window = |end: usize| {
                functions
                    .get(start..end)
                    .ok_or(Error::InvalidModel("segment partition is out of range"))
            };
            let mut best_bytes = self
                .builder_for_functions(window(best)?.iter().copied(), options)?
                .to_bytes()?;
            while low <= high {
                let middle = low.saturating_add(high.saturating_sub(low) / 2);
                let bytes = self
                    .builder_for_functions(window(middle)?.iter().copied(), options)?
                    .to_bytes()?;
                if bytes.len() <= target_size || middle == minimum {
                    best = middle;
                    best_bytes = bytes;
                    low = middle.saturating_add(1);
                } else {
                    high = middle.saturating_sub(1);
                }
            }
            let selected = window(best)?;
            let first = selected
                .first()
                .ok_or(Error::InvalidModel("segment partition is empty"))?;
            segments.push(GsymSegment {
                first_address: first.range.start,
                end_address: selected
                    .iter()
                    .map(|function| function.range.end)
                    .max()
                    .unwrap_or(first.range.end),
                function_count: selected.len(),
                bytes: best_bytes.into_boxed_slice(),
            });
            start = best;
        }
        Ok(segments)
    }

    fn builder_for_functions<'function>(
        &self,
        functions: impl Clone + IntoIterator<Item = &'function Function>,
        options: TranscodeOptions,
    ) -> Result<GsymBuilder> {
        if self
            .files
            .first()
            .is_none_or(|file| *file != FileEntry::default())
        {
            return Err(Error::InvalidModel("file-table index zero must be empty"));
        }
        let mut used = vec![false; self.files.len()];
        if let Some(zero) = used.first_mut() {
            *zero = true;
        }
        for function in functions.clone() {
            mark_function_files(function, &mut used)?;
        }
        let mut builder = GsymBuilder::new()
            .version(options.version.unwrap_or(self.source_version))
            .endian(options.endian.unwrap_or(self.source_endian))
            .base_address(self.base_address)
            .build_id(self.build_id.clone())
            .repair_zero_sized_functions(false);
        let mut remap = vec![FileIndex::ZERO; self.files.len()];
        for (old, file) in self.files.iter().enumerate().skip(1) {
            if used.get(old).copied().unwrap_or(false)
                && let Some(slot) = remap.get_mut(old)
            {
                *slot = builder.add_file(file.clone())?;
            }
        }
        for function in functions {
            let mut function = function.clone();
            remap_function_files(&mut function, &remap)?;
            builder.add_function(function)?;
        }
        Ok(builder)
    }
}

fn mark_file(index: FileIndex, used: &mut [bool]) -> Result<()> {
    let slot = used
        .get_mut(index.get() as usize)
        .ok_or(Error::InvalidModel("function references a missing file"))?;
    *slot = true;
    Ok(())
}

fn mark_inline_files(node: &InlineNode, used: &mut [bool]) -> Result<()> {
    mark_file(node.call_file, used)?;
    for child in &node.children {
        mark_inline_files(child, used)?;
    }
    Ok(())
}

fn mark_function_files(function: &Function, used: &mut [bool]) -> Result<()> {
    for line in &function.lines {
        mark_file(line.file, used)?;
    }
    if let Some(inline) = &function.inline {
        mark_inline_files(inline, used)?;
    }
    for merged in &function.merged {
        mark_function_files(merged, used)?;
    }
    Ok(())
}

fn remap_file(index: &mut FileIndex, remap: &[FileIndex]) -> Result<()> {
    *index = *remap
        .get(index.get() as usize)
        .ok_or(Error::InvalidModel("function references a missing file"))?;
    Ok(())
}

fn remap_inline_files(node: &mut InlineNode, remap: &[FileIndex]) -> Result<()> {
    remap_file(&mut node.call_file, remap)?;
    for child in &mut node.children {
        remap_inline_files(child, remap)?;
    }
    Ok(())
}

fn remap_function_files(function: &mut Function, remap: &[FileIndex]) -> Result<()> {
    for line in &mut function.lines {
        remap_file(&mut line.file, remap)?;
    }
    if let Some(inline) = &mut function.inline {
        remap_inline_files(inline, remap)?;
    }
    for merged in &mut function.merged {
        remap_function_files(merged, remap)?;
    }
    Ok(())
}

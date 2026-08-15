mod function;
mod layout;
mod lookup;
mod owned;

use std::fmt;
use std::path::Path;

use smallvec::SmallVec;
use zerocopy::byteorder::{BigEndian, LittleEndian, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout};

use crate::GsymVersion;
use crate::endian::{Cursor, Endian};
use crate::error::{Error, Result};
use crate::format::function::EncodedFunction;
use crate::model::{AddressRange, FileIndex};

pub use function::{FunctionRef, Functions};
use function::{RawFunction, file_at, string_at};
pub(crate) use layout::ParsedLayout;
use layout::VersionLayout;

/// Borrowed metadata from a parsed GSYM header.
///
/// The fields are normalized across versions, so the same struct describes a v1
/// and a v2 file.
///
/// Returned by [`Gsym::header`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Header<'data> {
    /// Format version.
    pub version: GsymVersion,
    /// Byte order used for fixed-width integers.
    pub endian: Endian,
    /// Width in bytes of each address-table entry.
    pub address_offset_size: u8,
    /// Base address added to every address-table entry.
    pub base_address: u64,
    /// Number of functions in the file.
    pub address_count: u32,
    /// Opaque build identifier borrowed from the input.
    pub build_id: &'data [u8],
}

/// Controls which optional records an allocating lookup inspects.
///
/// Every field defaults to `true`, which is what [`Gsym::lookup`] uses. Turning
/// a field off skips that work entirely, so a name-only lookup costs less than
/// a full one.
///
/// ```
/// use gsym::LookupOptions;
///
/// let names_only = LookupOptions {
///     line_information: false,
///     inline_frames: false,
///     call_sites: false,
/// };
/// assert!(LookupOptions::default().line_information);
/// assert!(!names_only.line_information);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LookupOptions {
    /// Resolve source files and lines.
    pub line_information: bool,
    /// Resolve inline-call frames.
    pub inline_frames: bool,
    /// Resolve matching call-site patterns.
    pub call_sites: bool,
}

impl Default for LookupOptions {
    fn default() -> Self {
        Self {
            line_information: true,
            inline_frames: true,
            call_sites: true,
        }
    }
}

/// Controls which optional records frame visitation inspects.
///
/// The same as [`LookupOptions`] without `call_sites`, which
/// [`Gsym::for_each_frame`] does not report. Converting to [`LookupOptions`]
/// leaves call sites disabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLookupOptions {
    /// Resolve source files and lines.
    pub line_information: bool,
    /// Resolve inline-call frames.
    pub inline_frames: bool,
}

impl Default for FrameLookupOptions {
    fn default() -> Self {
        Self {
            line_information: true,
            inline_frames: true,
        }
    }
}

impl From<FrameLookupOptions> for LookupOptions {
    fn from(options: FrameLookupOptions) -> Self {
        Self {
            line_information: options.line_information,
            inline_frames: options.inline_frames,
            call_sites: false,
        }
    }
}

/// Reusable buffer for allocation-free address lookup.
///
/// Reusing one scratch across lookups avoids allocating per address. It carries
/// no state between calls, so one per thread is enough.
///
/// Used by [`Gsym::lookup_with_options`] and [`Gsym::for_each_frame`].
#[derive(Default)]
pub struct LookupScratch {
    inline_frames: SmallVec<[RawInlineFrame; 4]>,
}

impl fmt::Debug for LookupScratch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LookupScratch")
            .finish_non_exhaustive()
    }
}

impl LookupScratch {
    /// Creates scratch storage preallocated for `inline_depth` nested frames.
    #[must_use]
    pub fn with_capacity(inline_depth: usize) -> Self {
        Self {
            inline_frames: SmallVec::with_capacity(inline_depth),
        }
    }

    fn clear(&mut self) {
        self.inline_frames.clear();
    }
}

/// Aggregate counts produced by full-file verification.
///
/// Returned by [`Gsym::verify`], which checks every function in the file and
/// everything it references.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct VerifyReport {
    /// Number of address-table entries verified.
    pub functions: usize,
    /// Number of file-table entries verified.
    pub files: usize,
    /// Number of NUL-terminated strings stored in the string table.
    pub strings: usize,
    /// Total `FunctionInfo` section size in bytes.
    pub function_info_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
struct RawInlineFrame {
    name: u64,
    call_file: FileIndex,
    call_line: u32,
    start: u64,
}

/// Parsed GSYM data backed by caller-selected byte storage.
///
/// `D` may be a borrowed slice or an owned type such as `Vec<u8>`,
/// `Box<[u8]>`, or `Arc<[u8]>`, and none of them is copied. With the `mmap`
/// feature, `MappedGsym` is this same type over a read-only memory map.
///
/// Construct one with [`Gsym::open`] for a path or [`Gsym::parse`] for bytes.
/// Both reject an unsupported version, a truncated file, or a section outside
/// the input. A malformed function record is reported when it is read, so
/// [`Gsym::verify`] is the way to check a whole file up front.
///
/// Lookups take `&self` and keep no interior state, so a reader can be shared
/// between threads whenever its storage is `Sync`.
///
/// # Borrowed and owned storage
///
/// ```
/// use gsym::{AddressRange, Function, Gsym, GsymBuilder};
///
/// let mut builder = GsymBuilder::new();
/// builder.add_function(Function::new(
///     AddressRange::new(0x2000, 0x2010),
///     b"borrowed",
/// ))?;
/// let bytes = builder.to_bytes()?;
///
/// let borrowed = Gsym::parse(bytes.as_slice())?;
/// assert_eq!(borrowed.as_ref().as_ptr(), bytes.as_ptr());
///
/// let owned = Gsym::parse(bytes)?;
/// assert_eq!(owned.lookup(0x2000)?.unwrap().frames()[0].name, b"borrowed");
/// # Ok::<(), gsym::Error>(())
/// ```
pub struct Gsym<D> {
    pub(super) data: D,
    pub(super) layout: ParsedLayout,
}

impl<D: AsRef<[u8]>> fmt::Debug for Gsym<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Gsym")
            .field("byte_len", &self.data.as_ref().len())
            .field("version", &self.header().version)
            .field("endian", &self.layout.endian)
            .field("base_address", &self.layout.base_address)
            .field("function_count", &self.layout.address_count)
            .field("build_id_len", &self.layout.build_id.len())
            .finish_non_exhaustive()
    }
}

impl Gsym<Vec<u8>> {
    /// Opens a GSYM file as an owned, immutable byte snapshot.
    ///
    /// This is the safe filesystem entry point. Use
    /// `MappedGsym::map` when demand paging is worth the file-stability
    /// contract of a memory map.
    ///
    /// # Errors
    ///
    /// Returns a contextual I/O error when the file cannot be read, or a
    /// format error when its GSYM metadata is invalid.
    ///
    /// ```no_run
    /// use gsym::Gsym;
    ///
    /// let gsym = Gsym::open("app.gsym")?;
    /// let symbol = gsym.lookup(0x401000)?;
    /// # Ok::<(), gsym::Error>(())
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path).map_err(|source| Error::IoAtPath {
            operation: "read GSYM file",
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(data)
    }
}

impl<D: AsRef<[u8]>> Gsym<D> {
    /// Parses and validates the top-level GSYM tables without copying them.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported version, truncated input, invalid
    /// table layout, or out-of-bounds section.
    pub fn parse(data: D) -> Result<Self> {
        let layout = layout::parse(data.as_ref())?;
        Ok(Self { data, layout })
    }

    /// Returns the caller-provided byte storage.
    #[must_use]
    pub fn into_inner(self) -> D {
        self.data
    }

    /// Returns decoded header metadata.
    #[must_use]
    pub fn header(&self) -> Header<'_> {
        Header {
            version: match self.layout.version {
                VersionLayout::V1 => GsymVersion::V1,
                VersionLayout::V2 => GsymVersion::V2,
            },
            endian: self.layout.endian,
            address_offset_size: self.layout.address_offset_size,
            base_address: self.layout.base_address,
            address_count: self.layout.address_count,
            build_id: self.build_id(),
        }
    }

    /// Returns the opaque build identifier, or an empty slice when absent.
    #[must_use]
    pub fn build_id(&self) -> &[u8] {
        self.data
            .as_ref()
            .get(self.layout.build_id.clone())
            .unwrap_or_default()
    }

    /// Iterates all indexed functions in address-table order.
    #[must_use]
    pub const fn functions(&self) -> Functions<'_, D> {
        Functions {
            gsym: self,
            next: 0,
        }
    }

    /// Returns a borrowed function record by address-table index.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FunctionIndexOutOfBounds`] when `index` does not exist,
    /// or a format error when its address or function header is malformed.
    pub fn function(&self, index: usize) -> Result<FunctionRef<'_>> {
        self.get_function(index)?
            .ok_or(Error::FunctionIndexOutOfBounds {
                index,
                count: self.layout.address_count as usize,
            })
    }

    /// Optionally returns a borrowed function record by address-table index.
    ///
    /// This is the non-erroring bounds-checking counterpart to [`Self::function`].
    ///
    /// # Errors
    ///
    /// Returns a format error if the indexed address or function header is
    /// malformed. An out-of-bounds index returns `Ok(None)`.
    pub fn get_function(&self, index: usize) -> Result<Option<FunctionRef<'_>>> {
        let Some(raw) = self.raw_function(index)? else {
            return Ok(None);
        };
        Ok(Some(FunctionRef {
            index,
            name: self.string(raw.name)?,
            all_data: self.data.as_ref(),
            raw,
            layout: &self.layout,
        }))
    }

    pub(in crate::reader) fn raw_function(&self, index: usize) -> Result<Option<RawFunction<'_>>> {
        if index >= self.layout.address_count as usize {
            return Ok(None);
        }
        let start = self.address(index)?;
        self.raw_function_at(index, start).map(Some)
    }

    /// Reads the record at `index`, whose start address the caller already has.
    ///
    /// Lookup walks a run of functions that share a start address, so passing
    /// it in saves re-decoding the same address-table entry per candidate.
    ///
    /// `index` must be below `address_count`.
    #[inline]
    pub(in crate::reader) fn raw_function_at(
        &self,
        index: usize,
        start: u64,
    ) -> Result<RawFunction<'_>> {
        let offset = self.function_offset(index)?;
        let section_end = self.layout.function_info.end;
        if offset < self.layout.function_info.start || offset >= section_end {
            return Err(Error::InvalidOffset {
                offset: offset as u64,
                input_len: self.data.as_ref().len(),
            });
        }
        let data =
            self.data
                .as_ref()
                .get(offset..section_end)
                .ok_or_else(|| Error::InvalidOffset {
                    offset: offset as u64,
                    input_len: self.data.as_ref().len(),
                })?;
        let mut header = Cursor::new(data, self.layout.endian);
        let size = header.read_u32()?;
        let name_offset = header.read_uint(self.layout.string_offset_size)?;
        if name_offset == 0 {
            return Err(Error::ZeroNameOffset);
        }
        let end = start
            .checked_add(u64::from(size))
            .ok_or(Error::Overflow("function range"))?;
        let raw = RawFunction {
            range: AddressRange::new(start, end),
            name: name_offset,
            data,
            records: data.get(header.position()..).ok_or(Error::InvalidFormat(
                "function record header overruns its record",
            ))?,
        };
        Ok(raw)
    }

    /// Resolves a string-table offset to borrowed bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-bounds offset or missing NUL terminator.
    pub fn string(&self, offset: u64) -> Result<&[u8]> {
        string_at(self.data.as_ref(), &self.layout.string_table, offset)
    }

    /// Resolves a file-table index to borrowed directory and basename bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid index or malformed string reference.
    pub fn file(&self, index: impl Into<FileIndex>) -> Result<(&[u8], &[u8])> {
        file_at(
            self.data.as_ref(),
            self.layout.endian,
            self.layout.string_offset_size,
            &self.layout.file_table,
            self.layout.file_count,
            &self.layout.string_table,
            index.into(),
        )
    }

    /// Fully verifies all indexed functions and their referenced metadata.
    ///
    /// Checks that the address table is sorted, that every function record
    /// decodes, and that the strings, files, line programs, and inline ranges
    /// they reference are in bounds and well formed. Cost is proportional to the
    /// file, so this belongs at load time for input of unknown provenance
    /// rather than in front of each lookup.
    ///
    /// # Errors
    ///
    /// Returns the first structural or semantic validation error.
    ///
    /// ```
    /// use gsym::{AddressRange, Function, Gsym, GsymBuilder};
    ///
    /// let mut builder = GsymBuilder::new();
    /// builder.add_function(Function::new(
    ///     AddressRange::new(0x1000, 0x1010),
    ///     b"verified",
    /// ))?;
    /// let bytes = builder.to_bytes()?;
    ///
    /// let report = Gsym::parse(&bytes)?.verify()?;
    /// assert_eq!(report.functions, 1);
    /// # Ok::<(), gsym::Error>(())
    /// ```
    pub fn verify(&self) -> Result<VerifyReport> {
        self.verify_with(|_, _| Ok(()))
    }

    pub(crate) fn decode_all_verified(&self) -> Result<(VerifyReport, Vec<crate::Function>)> {
        let mut functions = Vec::with_capacity(self.layout.address_count as usize);
        let report = self.verify_with(|reference, encoded| {
            functions.push(owned::decode(reference, encoded)?);
            Ok(())
        })?;
        Ok((report, functions))
    }

    fn verify_with(
        &self,
        mut visitor: impl FnMut(&FunctionRef<'_>, EncodedFunction) -> Result<()>,
    ) -> Result<VerifyReport> {
        if self
            .data
            .as_ref()
            .get(self.layout.string_table.start)
            .copied()
            != Some(0)
        {
            return Err(Error::InvalidFormat(
                "GSYM string table does not begin with an empty string",
            ));
        }
        if self.layout.file_count > 0 {
            let (directory, basename) = self.file(0_u32)?;
            if !directory.is_empty() || !basename.is_empty() {
                return Err(Error::InvalidFormat("file-table index zero must be empty"));
            }
        }
        let mut previous = None;
        for index in 0..self.layout.address_count as usize {
            let address = self.address(index)?;
            if previous.is_some_and(|value| address < value) {
                return Err(Error::InvalidFormat("address table is not sorted"));
            }
            previous = Some(address);
            let function = self.function(index)?;
            let decoded = function.decode_encoded()?;
            owned::validate(&function, &decoded)?;
            visitor(&function, decoded)?;
        }
        for index in 0..self.layout.file_count {
            let _ = self.file(index)?;
        }
        Ok(VerifyReport {
            functions: self.layout.address_count as usize,
            files: self.layout.file_count as usize,
            strings: self
                .data
                .as_ref()
                .get(self.layout.string_table.clone())
                .unwrap_or_default()
                .iter()
                .fold(0_usize, |total, byte| {
                    total.saturating_add(usize::from(*byte == 0))
                }),
            function_info_bytes: self.layout.function_info.len(),
        })
    }

    /// Reads one address-table entry.
    ///
    /// Single-entry reads use a cursor. Whole-table scans use typed slices in
    /// [`Self::find_address_index`].
    pub(super) fn address(&self, index: usize) -> Result<u64> {
        let width = usize::from(self.layout.address_offset_size);
        let offset = self
            .layout
            .address_offsets
            .start
            .checked_add(
                index
                    .checked_mul(width)
                    .ok_or(Error::Overflow("address table index"))?,
            )
            .ok_or(Error::Overflow("address table offset"))?;
        let mut cursor = Cursor::at(self.data.as_ref(), self.layout.endian, offset)?;
        self.layout
            .base_address
            .checked_add(cursor.read_uint(self.layout.address_offset_size)?)
            .ok_or(Error::Overflow("function address"))
    }

    fn function_offset(&self, index: usize) -> Result<usize> {
        let width: u8 = match self.layout.version {
            VersionLayout::V1 => 4,
            VersionLayout::V2 => 8,
        };
        let offset = self
            .layout
            .address_info_offsets
            .start
            .checked_add(
                index
                    .checked_mul(usize::from(width))
                    .ok_or(Error::Overflow("address-info table index"))?,
            )
            .ok_or(Error::Overflow("address-info table offset"))?;
        let mut cursor = Cursor::at(self.data.as_ref(), self.layout.endian, offset)?;
        let relative = cursor.read_uint(width)?;
        let absolute = match self.layout.version {
            VersionLayout::V1 => relative,
            VersionLayout::V2 => relative
                .checked_add(self.layout.function_info.start as u64)
                .ok_or(Error::Overflow("FunctionInfo offset"))?,
        };
        usize::try_from(absolute).map_err(|_| Error::Overflow("FunctionInfo offset conversion"))
    }

    pub(super) fn find_address_index(&self, address: u64) -> Result<Option<usize>> {
        if address < self.layout.base_address || self.layout.address_count == 0 {
            return Ok(None);
        }
        let count = self.layout.address_count as usize;
        let relative = address.saturating_sub(self.layout.base_address);
        let entries = self
            .data
            .as_ref()
            .get(self.layout.address_offsets.clone())
            .ok_or_else(|| Error::InvalidOffset {
                offset: self.layout.address_offsets.start as u64,
                input_len: self.data.as_ref().len(),
            })?;

        let low = match (self.layout.address_offset_size, self.layout.endian) {
            (1, _) => partition_point::<1>(entries, relative, |entry| u64::from(entry[0])),
            (2, Endian::Little) => {
                typed_partition_point::<U16<LittleEndian>>(entries, relative, |entry| {
                    u64::from(entry.get())
                })?
            }
            (2, Endian::Big) => {
                typed_partition_point::<U16<BigEndian>>(entries, relative, |entry| {
                    u64::from(entry.get())
                })?
            }
            (4, Endian::Little) => {
                typed_partition_point::<U32<LittleEndian>>(entries, relative, |entry| {
                    u64::from(entry.get())
                })?
            }
            (4, Endian::Big) => {
                typed_partition_point::<U32<BigEndian>>(entries, relative, |entry| {
                    u64::from(entry.get())
                })?
            }
            (8, Endian::Little) => {
                typed_partition_point::<U64<LittleEndian>>(entries, relative, |entry| entry.get())?
            }
            (8, Endian::Big) => {
                typed_partition_point::<U64<BigEndian>>(entries, relative, |entry| entry.get())?
            }
            _ => {
                let mut low = 0usize;
                let mut high = count;
                while low < high {
                    let middle = low.saturating_add(high.saturating_sub(low) / 2);
                    if self.address(middle)? <= address {
                        low = middle.saturating_add(1);
                    } else {
                        high = middle;
                    }
                }
                low
            }
        };
        Ok(low.checked_sub(1))
    }
}

#[inline]
fn partition_point<const N: usize>(
    entries: &[u8],
    probe: u64,
    decode: impl Fn([u8; N]) -> u64,
) -> usize {
    let (chunks, _) = entries.as_chunks::<N>();
    chunks.partition_point(|entry| decode(*entry) <= probe)
}

#[inline]
fn typed_partition_point<T>(entries: &[u8], probe: u64, decode: impl Fn(&T) -> u64) -> Result<usize>
where
    [T]: FromBytes + KnownLayout + Immutable,
{
    let entries = <[T]>::ref_from_bytes(entries)
        .map_err(|_| Error::InvalidFormat("address table has an invalid typed layout"))?;
    Ok(entries.partition_point(|entry| decode(entry) <= probe))
}

impl<D: AsRef<[u8]>> AsRef<[u8]> for Gsym<D> {
    fn as_ref(&self) -> &[u8] {
        self.data.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{AddressRange, FileEntry, Function, InlineNode};
    use crate::{Error, GsymBuilder};

    use super::Gsym;

    #[test]
    fn verification_rejects_a_non_empty_reserved_file_entry() {
        let mut builder = GsymBuilder::new();
        let _ = builder.add_file(FileEntry::new("/src", "main.c")).unwrap();
        builder
            .add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"main"))
            .unwrap();
        let mut bytes = builder.to_bytes().unwrap();

        let table = Gsym::parse(bytes.as_slice()).unwrap().layout.file_table;
        let reserved = table.start.saturating_add(4);
        let first = reserved.saturating_add(8);
        bytes.copy_within(first..first.saturating_add(8), reserved);

        assert!(matches!(
            Gsym::parse(bytes.as_slice()).unwrap().verify(),
            Err(Error::InvalidFormat("file-table index zero must be empty"))
        ));
    }

    #[test]
    fn verification_counts_stored_strings() {
        let mut builder = GsymBuilder::new();
        let _ = builder.add_file(FileEntry::new("/src", "main.c")).unwrap();
        builder
            .add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"main"))
            .unwrap();
        builder
            .add_function(Function::new(AddressRange::new(0x2000, 0x2010), b"helper"))
            .unwrap();
        let bytes = builder.to_bytes().unwrap();

        let report = Gsym::parse(bytes.as_slice()).unwrap().verify().unwrap();
        assert_eq!(report.functions, 2);
        assert_eq!(report.files, 2);
        assert_eq!(report.strings, 5);
    }

    #[test]
    fn verification_and_owned_decode_reject_a_missing_inline_file() {
        let range = AddressRange::new(0x1000, 0x1010);
        let mut builder = GsymBuilder::new();
        builder
            .add_function(Function {
                inline: Some(InlineNode {
                    ranges: vec![range],
                    name: b"inlined".to_vec(),
                    call_file: 0_u32.into(),
                    ..InlineNode::default()
                }),
                ..Function::new(range, b"outer")
            })
            .unwrap();
        let mut bytes = builder.to_bytes().unwrap();
        let function_offset = Gsym::parse(bytes.as_slice())
            .unwrap()
            .function_offset(0)
            .unwrap();
        let inline_payload = function_offset.saturating_add(16);
        let call_file = inline_payload.saturating_add(8);
        let Some(slot) = bytes.get_mut(call_file) else {
            panic!("writer omitted the inline call-file field");
        };
        *slot = 2;

        let gsym = Gsym::parse(bytes.as_slice()).unwrap();
        assert!(gsym.verify().is_err());
        assert!(gsym.function(0).unwrap().decode().is_err());
    }
}

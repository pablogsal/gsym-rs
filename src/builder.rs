use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::io::Write;

use hashbrown::HashTable;

use crate::model::{AddressRange, FileEntry, FileIndex, Function};
use crate::validation::validate_for_builder;
use crate::writer::WriterOptions;
use crate::{Error, GsymVersion, Result};

/// How finalization treats functions that share an address range.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum FunctionSetPolicy {
    /// Keep the richest record of each equal-range group.
    #[default]
    Deduplicate,
    /// Keep equal-range aliases as `MergedFunctionsInfo` records.
    MergeEqualRanges,
    /// Encode every function as supplied.
    Preserve,
}

/// Finalization policy used by [`GsymBuilder`].
///
/// [`Default`] enables `repair_zero_sized_functions` and disables
/// `merge_equal_address_functions`, matching what `llvm-gsymutil` does without
/// extra flags. The [`GsymBuilder`] setters change the same fields one at a
/// time.
#[derive(Clone, Eq, PartialEq)]
pub struct BuilderOptions {
    /// Wire-format settings used by the writer.
    pub writer: WriterOptions,
    /// Executable virtual-address ranges, used to reject stale DWARF and to
    /// repair zero-sized symbol-table functions.
    pub executable_ranges: Box<[AddressRange]>,
    /// Extend the final zero-sized function to its containing executable range.
    pub repair_zero_sized_functions: bool,
    /// Preserve equal-range aliases as `MergedFunctionsInfo`. This mirrors
    /// llvm-gsymutil's explicit merged-functions mode and is off by default.
    pub merge_equal_address_functions: bool,
}

impl fmt::Debug for BuilderOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuilderOptions")
            .field("writer", &self.writer)
            .field("executable_range_count", &self.executable_ranges.len())
            .field(
                "repair_zero_sized_functions",
                &self.repair_zero_sized_functions,
            )
            .field(
                "merge_equal_address_functions",
                &self.merge_equal_address_functions,
            )
            .finish()
    }
}

impl Default for BuilderOptions {
    fn default() -> Self {
        Self {
            writer: WriterOptions::default(),
            executable_ranges: Box::default(),
            repair_zero_sized_functions: true,
            merge_equal_address_functions: false,
        }
    }
}

/// Version-independent, deterministic GSYM construction API.
///
/// Add [`FileEntry`] values and [`Function`] records, then encode with
/// [`Self::to_bytes`] or [`Self::write_to`]. The same inputs and options always
/// produce the same bytes, so output can be compared or content-addressed.
///
/// Two things to know while building:
///
/// - [`Self::add_file`] interns entries. Adding the same directory and basename
///   twice returns the same [`FileIndex`], and index zero is permanently the
///   empty entry.
/// - [`Self::add_function`] validates as it goes and rejects an empty name, a
///   reversed range, or a line row outside the function. Cross-record checks
///   that need the whole model, such as file references, run at encode time.
///
/// Functions may be added in any order. Setters take and return `self`, so they
/// chain, while `add_file` and `add_function` take `&mut self`.
///
/// # Example
///
/// ```
/// use gsym::{AddressRange, Function, Gsym, GsymBuilder};
///
/// let mut builder = GsymBuilder::new().base_address(0x1000);
/// builder.add_function(Function::new(
///     AddressRange::new(0x1010, 0x1020),
///     b"example",
/// ))?;
///
/// let bytes = builder.to_bytes()?;
/// let gsym = Gsym::parse(bytes)?;
/// assert_eq!(gsym.lookup(0x1014)?.unwrap().frames()[0].name, b"example");
/// # Ok::<(), gsym::Error>(())
/// ```
pub struct GsymBuilder {
    options: BuilderOptions,
    function_set: FunctionSetPolicy,
    files: Vec<FileEntry>,
    file_index: HashTable<FileSlot>,
    hasher: RandomState,
    functions: Vec<Function>,
}

#[derive(Clone, Copy, Debug)]
struct FileSlot {
    index: u32,
    hash: u64,
}

fn interned_file(files: &[FileEntry], index: u32) -> Option<&FileEntry> {
    files.get(usize::try_from(index).ok()?)
}

impl fmt::Debug for GsymBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GsymBuilder")
            .field("options", &self.options)
            .field("file_count", &self.files.len())
            .field("function_count", &self.functions.len())
            .finish_non_exhaustive()
    }
}

impl Default for GsymBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GsymBuilder {
    /// Creates an empty builder using default options.
    #[must_use]
    pub fn new() -> Self {
        let hasher = RandomState::new();
        let empty = FileEntry::default();
        let slot = FileSlot {
            index: 0,
            hash: hasher.hash_one(&empty),
        };
        let mut file_index = HashTable::new();
        file_index.insert_unique(slot.hash, slot, |slot| slot.hash);
        Self {
            options: BuilderOptions::default(),
            function_set: FunctionSetPolicy::Deduplicate,
            files: vec![empty],
            file_index,
            hasher,
            functions: Vec::new(),
        }
    }

    /// Creates an empty builder using `options`.
    #[must_use]
    pub fn with_options(options: BuilderOptions) -> Self {
        let mut builder = Self::new();
        builder.function_set = if options.merge_equal_address_functions {
            FunctionSetPolicy::MergeEqualRanges
        } else {
            FunctionSetPolicy::Deduplicate
        };
        builder.options = options;
        builder
    }

    /// Returns the active builder options.
    #[must_use]
    pub const fn options(&self) -> &BuilderOptions {
        &self.options
    }

    /// Selects the output GSYM version.
    ///
    /// Defaults to [`GsymVersion::V1`], which current tooling reads. Selecting
    /// [`GsymVersion::V2`] lifts v1's 4 GiB offset limits and 20-byte build-ID
    /// limit but needs LLVM 23 or newer on the reading side. Encoding reports
    /// v1 limit errors and does not change versions automatically.
    #[must_use]
    pub const fn version(mut self, version: GsymVersion) -> Self {
        self.options.writer.version = version;
        self
    }

    /// Selects the output byte order.
    #[must_use]
    pub const fn endian(mut self, endian: crate::Endian) -> Self {
        self.options.writer.endian = endian;
        self
    }

    /// Sets the image base address.
    ///
    /// Leave it unset to use the lowest function address, which is what a
    /// standalone file wants. Set it to the base address of the image the data
    /// came from, so lookups can use that image's virtual addresses.
    #[must_use]
    pub const fn base_address(mut self, address: u64) -> Self {
        self.options.writer.base_address = Some(address);
        self
    }

    /// Sets the opaque build identifier stored in the GSYM header.
    #[must_use]
    pub fn build_id(mut self, build_id: impl Into<Vec<u8>>) -> Self {
        self.options.writer.build_id = build_id.into();
        self
    }

    /// Enables or disables final zero-sized-function repair.
    #[must_use]
    pub const fn repair_zero_sized_functions(mut self, enabled: bool) -> Self {
        self.options.repair_zero_sized_functions = enabled;
        self
    }

    /// Selects how functions sharing an address range are finalized.
    #[must_use]
    pub const fn function_set(mut self, policy: FunctionSetPolicy) -> Self {
        self.options.merge_equal_address_functions =
            matches!(policy, FunctionSetPolicy::MergeEqualRanges);
        self.function_set = policy;
        self
    }

    /// Enables or disables merged records for equal-address functions.
    #[must_use]
    pub const fn merge_equal_address_functions(mut self, enabled: bool) -> Self {
        self.options.merge_equal_address_functions = enabled;
        self.function_set = if enabled {
            FunctionSetPolicy::MergeEqualRanges
        } else {
            FunctionSetPolicy::Deduplicate
        };
        self
    }

    /// Returns how equal-range functions will be finalized.
    #[must_use]
    pub const fn function_set_policy(&self) -> FunctionSetPolicy {
        self.function_set
    }

    /// Replaces the executable ranges used for liveness and size repair.
    #[must_use]
    pub fn executable_ranges(mut self, ranges: impl IntoIterator<Item = AddressRange>) -> Self {
        self.options.executable_ranges = ranges.into_iter().collect::<Vec<_>>().into_boxed_slice();
        self
    }

    /// Intern a source file and return its stable one-based index. Index zero
    /// is permanently reserved for the empty file.
    ///
    /// Repeated calls with an equal entry return the same index without adding
    /// a row, so callers can intern per line row instead of maintaining their
    /// own map.
    ///
    /// # Errors
    ///
    /// Returns an error if the table exceeds the GSYM `u32` index space.
    ///
    /// ```
    /// use gsym::{FileEntry, GsymBuilder};
    ///
    /// let mut builder = GsymBuilder::new();
    /// let first = builder.add_file(FileEntry::new(b"/src", b"main.rs"))?;
    /// let again = builder.add_file(FileEntry::new(b"/src", b"main.rs"))?;
    /// assert_eq!(first, again);
    /// assert_eq!(builder.files().len(), 2); // reserved entry plus main.rs
    /// # Ok::<(), gsym::Error>(())
    /// ```
    pub fn add_file(&mut self, file: FileEntry) -> Result<FileIndex> {
        let hash = self.hasher.hash_one(&file);
        if let Some(slot) = self.file_index.find(hash, |slot| {
            interned_file(&self.files, slot.index) == Some(&file)
        }) {
            return Ok(FileIndex::new(slot.index));
        }
        let index = u32::try_from(self.files.len()).map_err(|_| Error::Limit {
            context: "file table",
            value: self.files.len() as u64,
            limit: u64::from(u32::MAX),
        })?;
        self.files.push(file);
        self.file_index
            .insert_unique(hash, FileSlot { index, hash }, |slot| slot.hash);
        Ok(FileIndex::new(index))
    }

    /// Adds a validated function record.
    ///
    /// Insertion order does not matter. Line rows must already be sorted within
    /// the function, and inline ranges must nest inside their parent.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty name, invalid range, oversized function,
    /// or line outside the function range.
    pub fn add_function(&mut self, function: Function) -> Result<()> {
        validate_for_builder(&function)?;
        self.functions.push(function);
        Ok(())
    }

    /// Returns the interned file table, including reserved index zero.
    #[must_use]
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    /// Returns functions in insertion order before writer finalization.
    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    /// Encodes this builder into `output`.
    ///
    /// # Errors
    ///
    /// Returns an error when the model cannot be represented by the selected
    /// GSYM version or when writing fails.
    pub fn write_to(self, output: impl Write) -> Result<()> {
        crate::writer::write_builder(self, output)
    }

    /// Encodes this builder into a byte vector.
    ///
    /// # Errors
    ///
    /// Returns an error when the model cannot be represented by the selected
    /// GSYM version.
    pub fn to_bytes(self) -> Result<Vec<u8>> {
        crate::writer::encode_builder_to_bytes(self)
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        BuilderOptions,
        FunctionSetPolicy,
        Vec<FileEntry>,
        Vec<Function>,
    ) {
        (self.options, self.function_set, self.files, self.functions)
    }
}

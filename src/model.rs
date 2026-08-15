/// A half-open virtual-address range, `[start, end)`.
///
/// Addresses are the unslid virtual addresses of the image the data describes,
/// so a runtime address must have its load bias removed before it is compared
/// against a range. See [`docs::symbolication`](crate::docs::symbolication).
///
/// An empty range is legal and means a function whose size the producer did not
/// know. [`Self::contains`] reports `false` for every address in that case, but
/// address lookup still resolves such a function, for addresses from its start
/// up to the next function.
///
/// ```
/// use gsym::AddressRange;
///
/// let range = AddressRange::new(0x1000, 0x1020);
/// assert_eq!(range.size(), 0x20);
/// assert!(range.contains(0x1000));
/// assert!(!range.contains(0x1020));
///
/// // Endpoints are not checked on construction.
/// let reversed = AddressRange::new(0x20, 0x10);
/// assert!(!reversed.is_valid());
/// assert_eq!(reversed.size(), 0);
/// assert!(reversed.is_empty());
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub struct AddressRange {
    /// Inclusive start address.
    pub start: u64,
    /// Exclusive end address.
    pub end: u64,
}

impl AddressRange {
    /// Creates a range without validating endpoint order.
    #[must_use]
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// Returns the range width, or zero for reversed endpoints.
    #[must_use]
    pub const fn size(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Returns whether the start is less than or equal to the end.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.start <= self.end
    }

    /// Returns whether the range covers no addresses, including reversed ones.
    ///
    /// This is defined as [`Self::size`] being zero, so it is also true for the
    /// reversed endpoints [`Self::is_valid`] rejects: such a range covers
    /// nothing, and [`Self::contains`] reports `false` for every address in it.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.size() == 0
    }

    /// Returns whether `address` lies in this valid half-open range.
    #[must_use]
    pub const fn contains(self, address: u64) -> bool {
        self.is_valid() && self.start <= address && address < self.end
    }

    /// Returns whether this valid range fully contains `other`.
    #[must_use]
    pub const fn contains_range(self, other: Self) -> bool {
        self.is_valid() && other.is_valid() && self.start <= other.start && other.end <= self.end
    }
}

/// A source file split into directory and basename, as GSYM stores it.
///
/// Neither half is required to be valid UTF-8, and neither is normalized: what
/// the producer wrote is what a reader gets back.
///
/// [`GsymBuilder::add_file`](crate::GsymBuilder::add_file) interns entries and
/// returns the [`FileIndex`] that line rows and inline nodes refer to.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct FileEntry {
    /// Directory component as raw string-table bytes.
    pub directory: Vec<u8>,
    /// Basename component as raw string-table bytes.
    pub basename: Vec<u8>,
}

impl FileEntry {
    /// Creates a source-file entry from raw directory and basename bytes.
    #[must_use]
    pub fn new(directory: impl Into<Vec<u8>>, basename: impl Into<Vec<u8>>) -> Self {
        Self {
            directory: directory.into(),
            basename: basename.into(),
        }
    }
}

/// An index into a GSYM file table.
///
/// Index [`ZERO`](Self::ZERO) is reserved for the empty file entry, so real
/// files are numbered from 1. A line row that carries index zero has no known
/// source file rather than a file named `""`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct FileIndex(u32);

impl FileIndex {
    /// Reserved index representing the empty file entry.
    pub const ZERO: Self = Self(0);

    /// Wraps a raw file-table index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the raw file-table index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for FileIndex {
    fn from(index: u32) -> Self {
        Self::new(index)
    }
}

impl From<FileIndex> for u32 {
    fn from(index: FileIndex) -> Self {
        index.get()
    }
}

impl From<FileIndex> for u64 {
    fn from(index: FileIndex) -> Self {
        Self::from(index.get())
    }
}

/// One address-to-source-row mapping.
///
/// A row stays in effect from its address until the next row's address, so a
/// function's rows must be sorted and must start at or after the function's
/// start address. The writer rejects rows that violate either rule.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct LineEntry {
    /// Unslid virtual address at which this row becomes active.
    pub address: u64,
    /// Source file-table index.
    pub file: FileIndex,
    /// One-based source line number, or zero when unknown.
    pub line: u32,
}

impl LineEntry {
    /// Creates a source-line row.
    #[must_use]
    pub const fn new(address: u64, file: FileIndex, line: u32) -> Self {
        Self {
            address,
            file,
            line,
        }
    }
}

/// Recursive inline-call information for a function.
///
/// The root node covers the function itself, and each child covers the address
/// ranges occupied by one inlined call inside its parent. A child's ranges must
/// be contained by its parent's, and sibling ranges must not overlap.
///
/// `call_file` and `call_line` describe where the call appears in the *parent*,
/// not where the inlined body was defined. Address lookup reads them from the
/// callee to give each outer frame in a [`Lookup`] its source position.
#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct InlineNode {
    /// Sorted, disjoint address ranges covered by this inline invocation.
    pub ranges: Vec<AddressRange>,
    /// Function name as raw string-table bytes.
    pub name: Vec<u8>,
    /// File containing the call site in the parent frame.
    pub call_file: FileIndex,
    /// Source line containing the call site in the parent frame.
    pub call_line: u32,
    /// Nested inline invocations.
    pub children: Vec<Self>,
}

/// Metadata for a call instruction's return address.
///
/// [`match_regex`](Self::match_regex) names the callees that may return to this
/// address, so a stack walker can check it against the frame above. This crate
/// stores and returns the patterns without interpreting them.
#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct CallSite {
    /// Return-address offset relative to the containing function start.
    pub return_offset: u64,
    /// Classification bits retained from the input.
    pub flags: CallSiteFlags,
    /// Regular-expression strings describing possible callees.
    pub match_regex: Vec<Vec<u8>>,
}

/// Forward-compatible GSYM call-site flag bits.
///
/// Bits this crate does not define are kept rather than cleared, so they
/// survive a decode and re-encode. Use
/// [`from_bits_retain`](Self::from_bits_retain) to construct a value from a raw
/// byte and [`bits`](Self::bits) to get it back.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct CallSiteFlags(u8);

impl CallSiteFlags {
    /// Call target is internal to the image.
    pub const INTERNAL: Self = Self(1 << 0);
    /// Call target may be external to the image.
    pub const EXTERNAL: Self = Self(1 << 1);

    /// Wraps all bits, including values unknown to this crate.
    #[must_use]
    pub const fn from_bits_retain(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw flag byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether no bits are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns whether every bit in `flag` is present.
    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
}

impl std::ops::BitOr for CallSiteFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for CallSiteFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl From<u8> for CallSiteFlags {
    fn from(value: u8) -> Self {
        Self::from_bits_retain(value)
    }
}

impl From<CallSiteFlags> for u8 {
    fn from(value: CallSiteFlags) -> Self {
        value.bits()
    }
}

/// Owned semantic function data accepted by a GSYM builder.
///
/// Only [`range`](Self::range) and [`name`](Self::name) are required;
/// [`Function::new`] fills in the rest as empty. A function with no line rows
/// and no inline tree still resolves an address to a name and an offset.
///
/// ```
/// use gsym::{AddressRange, Function, LineEntry, FileIndex};
///
/// let plain = Function::new(AddressRange::new(0x1000, 0x1010), b"plain");
/// assert!(plain.lines.is_empty());
///
/// let with_lines = Function {
///     lines: vec![LineEntry::new(0x1000, FileIndex::new(1), 42)],
///     ..Function::new(AddressRange::new(0x1000, 0x1010), b"detailed")
/// };
/// assert_eq!(with_lines.lines.len(), 1);
/// ```
///
/// [`merged`](Self::merged) holds aliases that share this function's address
/// range, which identical-code folding produces. They are written only when
/// [`FunctionSetPolicy::MergeEqualRanges`](crate::FunctionSetPolicy) is
/// enabled.
#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Function {
    /// Function address range.
    pub range: AddressRange,
    /// Function name as raw string-table bytes.
    pub name: Vec<u8>,
    /// Sorted source-line rows belonging to this function.
    pub lines: Vec<LineEntry>,
    /// Root of the inline-call tree, when present.
    pub inline: Option<InlineNode>,
    /// Equal-range aliases stored as merged `FunctionInfo` records.
    pub merged: Vec<Self>,
    /// Call-site metadata for this function.
    pub call_sites: Vec<CallSite>,
}

impl Function {
    /// Creates a function with no optional line, inline, merged, or call-site data.
    #[must_use]
    pub fn new(range: AddressRange, name: impl Into<Vec<u8>>) -> Self {
        Self {
            range,
            name: name.into(),
            ..Self::default()
        }
    }
}

/// One source frame returned by address lookup.
///
/// Every field borrows from the GSYM input, so a frame cannot outlive the
/// reader it came from. Names and paths are raw bytes and are not checked for
/// UTF-8.
///
/// [`line`](Self::line) and the two path fields describe this frame's own
/// position. For the innermost frame that is the line row covering the looked-up
/// address; for an outer frame it is the call site recorded by the frame nested
/// inside it. A zero line and empty paths mean the file has no line information
/// for the address, or that the caller disabled it in
/// [`LookupOptions`](crate::LookupOptions).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct LookupFrame<'data> {
    /// Function name borrowed from the string table.
    pub name: &'data [u8],
    /// Source directory borrowed from the string table.
    pub directory: &'data [u8],
    /// Source basename borrowed from the string table.
    pub basename: &'data [u8],
    /// Source line, or zero when unavailable.
    pub line: u32,
    /// Address offset from the beginning of this frame's function or inline range.
    pub offset: u64,
    /// Whether this frame represents an inline invocation.
    pub inlined: bool,
}

/// Borrowed symbolication result for one address.
///
/// [`frames`](Self::frames) is ordered innermost first and always holds at
/// least one frame. Without inlining that is the only frame. With inlining,
/// frame 0 is the deepest inlined body containing the address, the last frame
/// is the function the linker emitted, and the frames between them are the
/// inlined calls, so printing them in order gives the call stack.
///
/// [`call_site_patterns`](Self::call_site_patterns) is non-empty only when the
/// looked-up address is exactly a recorded return address and call-site records
/// were requested.
///
/// ```
/// use gsym::{AddressRange, Function, Gsym, GsymBuilder};
///
/// let mut builder = GsymBuilder::new();
/// builder.add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"f"))?;
/// let bytes = builder.to_bytes()?;
/// let gsym = Gsym::parse(&bytes)?;
///
/// let hit = gsym.lookup(0x1008)?.expect("covered address");
/// assert_eq!(hit.address, 0x1008);
/// assert_eq!(hit.function, AddressRange::new(0x1000, 0x1010));
/// assert_eq!(hit.frames().last().unwrap().name, b"f");
/// # Ok::<(), gsym::Error>(())
/// ```
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Lookup<'data> {
    /// Address supplied by the caller.
    pub address: u64,
    /// Address range of the selected top-level function.
    pub function: AddressRange,
    frames: Box<[LookupFrame<'data>]>,
    call_site_patterns: Box<[&'data [u8]]>,
}

impl<'data> Lookup<'data> {
    pub(crate) const fn new(
        address: u64,
        function: AddressRange,
        frames: Box<[LookupFrame<'data>]>,
        call_site_patterns: Box<[&'data [u8]]>,
    ) -> Self {
        Self {
            address,
            function,
            frames,
            call_site_patterns,
        }
    }

    /// Returns resolved frames ordered from innermost to outermost.
    #[must_use]
    pub fn frames(&self) -> &[LookupFrame<'data>] {
        &self.frames
    }

    /// Returns call-site callee patterns for an exactly matching return address.
    #[must_use]
    pub fn call_site_patterns(&self) -> &[&'data [u8]] {
        &self.call_site_patterns
    }
}

use thiserror::Error;

#[cfg(feature = "convert")]
/// Identifies an ELF input in conversion diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElfInputKind {
    /// Linked executable or shared-object image.
    Image,
    /// Main DWARF-bearing debug file.
    Debug,
    /// Symbol-table input.
    Symbols,
    /// Supplementary DWARF object referenced by the main debug file.
    Supplementary,
    /// Packaged split-DWARF file.
    Dwp,
    /// Individual split-DWARF object.
    Dwo,
    /// Mini debug ELF extracted from `.gnu_debugdata`.
    EmbeddedDebugData,
}

#[cfg(feature = "convert")]
impl std::fmt::Display for ElfInputKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Image => "ELF image",
            Self::Debug => "ELF debug file",
            Self::Symbols => "ELF symbol file",
            Self::Supplementary => "supplementary ELF debug file",
            Self::Dwp => "DWP file",
            Self::Dwo => "DWO file",
            Self::EmbeddedDebugData => ".gnu_debugdata ELF",
        })
    }
}

#[cfg(feature = "convert")]
/// Property by which a companion ELF disagrees with its linked image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompanionMismatch {
    /// Machine architecture, address size, or byte order differs.
    Architecture,
    /// GNU build ID differs.
    BuildId,
}

#[cfg(feature = "convert")]
impl std::fmt::Display for CompanionMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Architecture => "architecture",
            Self::BuildId => "build ID",
        })
    }
}

/// Errors produced while parsing, building, or writing GSYM data.
///
/// Variants fall into a few groups. Format errors report malformed input
/// ([`InvalidMagic`](Self::InvalidMagic),
/// [`UnexpectedEof`](Self::UnexpectedEof),
/// [`InvalidFormat`](Self::InvalidFormat) and friends). Model errors report an
/// invalid value handed to the builder or writer
/// ([`InvalidModel`](Self::InvalidModel),
/// [`V1LimitExceeded`](Self::V1LimitExceeded)). The rest cover I/O and, with
/// the `convert` feature, ELF and DWARF diagnostics.
///
/// Matching on it needs a fallback arm.
///
/// ```
/// use gsym::{Error, Gsym};
///
/// match Gsym::parse(&[0_u8; 48][..]) {
///     Ok(_) => unreachable!("not a GSYM file"),
///     // "Not GSYM at all" is often worth treating as a normal answer.
///     Err(Error::InvalidMagic(_)) => {}
///     Err(other) => return Err(other),
/// }
/// # Ok::<(), gsym::Error>(())
/// ```
///
/// `Display` renders the full context, so `{error}` is enough for a log line.
/// Wrapped causes are available through
/// [`std::error::Error::source`](std::error::Error::source).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A read required bytes beyond the end of the input.
    #[error("unexpected end of input at offset {offset}: need {needed} bytes, have {remaining}")]
    UnexpectedEof {
        /// Byte offset at which the read began.
        offset: usize,
        /// Number of bytes requested.
        needed: usize,
        /// Number of bytes still available.
        remaining: usize,
    },

    /// Integer arithmetic overflowed while computing the named value.
    #[error("integer overflow while computing {0}")]
    Overflow(&'static str),

    /// A numeric field exceeded its format-defined maximum.
    #[error("invalid {field} value {value}; maximum is {max}")]
    OutOfRange {
        /// Name of the invalid field.
        field: &'static str,
        /// Observed value.
        value: u64,
        /// Inclusive maximum accepted value.
        max: u64,
    },

    /// A requested address-table function index does not exist.
    #[error("function index {index} is out of bounds for {count} functions")]
    FunctionIndexOutOfBounds {
        /// Requested zero-based index.
        index: usize,
        /// Number of indexed functions.
        count: usize,
    },

    /// The input does not begin with the GSYM magic in either byte order.
    #[error("invalid GSYM magic 0x{0:08x}")]
    InvalidMagic(u32),

    /// The input uses a GSYM version this crate does not implement.
    #[error("unsupported GSYM version {0}")]
    UnsupportedVersion(u16),

    /// The address-offset width is illegal for the selected format version.
    #[error("invalid address offset size {size} for GSYM v{version}")]
    InvalidAddressOffsetSize {
        /// GSYM version whose width rules apply.
        version: u16,
        /// Encoded width in bytes.
        size: u8,
    },

    /// GSYM v2 requested an unsupported string-table encoding.
    #[error("unsupported string table encoding {0}")]
    UnsupportedStringTableEncoding(u8),

    /// A GSYM v1 build identifier exceeds its fixed 20-byte field.
    #[error("invalid UUID size {0}; GSYM v1 supports at most 20 bytes")]
    InvalidUuidSize(usize),

    /// A requested GSYM v1 build identifier exceeds its fixed 20-byte field.
    #[error("GSYM v1 build identifier is {size} bytes; maximum is 20 bytes")]
    V1BuildIdTooLong {
        /// Requested build-identifier size.
        size: usize,
    },

    /// A required GSYM v2 global-data section is absent.
    #[error("missing required GSYM section type {0}")]
    MissingSection(u32),

    /// A GSYM v2 global-data section type appears more than once.
    #[error("duplicate GSYM section type {0}")]
    DuplicateSection(u32),

    /// A required GSYM v2 section has an empty byte range.
    #[error("GSYM section type {section_type} has zero size")]
    ZeroSizedSection {
        /// Numeric global-data section type.
        section_type: u32,
    },

    /// A GSYM v2 section extends outside the input.
    #[error(
        "GSYM section type {section_type} is outside the input: offset={offset}, size={size}, input={input_len}"
    )]
    SectionOutOfBounds {
        /// Numeric global-data section type.
        section_type: u32,
        /// Absolute file offset.
        offset: u64,
        /// Declared section size.
        size: u64,
        /// Actual input length.
        input_len: usize,
    },

    /// A referenced absolute file offset is outside the input.
    #[error("invalid offset {offset} for input of {input_len} bytes")]
    InvalidOffset {
        /// Referenced file offset.
        offset: u64,
        /// Actual input length.
        input_len: usize,
    },

    /// A requested binary alignment is zero or unsupported.
    #[error("invalid alignment {0}")]
    InvalidAlignment(usize),

    /// An unsigned LEB128 value is truncated, overlong, or too wide.
    #[error("malformed unsigned LEB128 at offset {offset}: {reason}")]
    MalformedUleb {
        /// Offset of the first encoded byte.
        offset: usize,
        /// Static validation failure description.
        reason: &'static str,
    },

    /// A signed LEB128 value is truncated, overlong, or too wide.
    #[error("malformed signed LEB128 at offset {offset}: {reason}")]
    MalformedSleb {
        /// Offset of the first encoded byte.
        offset: usize,
        /// Static validation failure description.
        reason: &'static str,
    },

    /// Full decoding encountered a `FunctionInfo` record type it cannot preserve.
    #[error("unsupported FunctionInfo type {0}")]
    UnsupportedInfoType(u32),

    /// A function or inline record references string-table offset zero as its name.
    #[error("FunctionInfo name offset must not be zero")]
    ZeroNameOffset,

    /// The input violates a GSYM format rule.
    #[error("invalid GSYM data: {0}")]
    InvalidFormat(&'static str),

    /// A value handed to the builder or writer is not valid.
    #[error("invalid GSYM model: {0}")]
    InvalidModel(&'static str),

    /// A count, size, or nesting depth exceeds an implementation or wire limit.
    #[error("{context} value {value} exceeds the supported limit of {limit}")]
    Limit {
        /// Value being bounded.
        context: &'static str,
        /// Observed value.
        value: u64,
        /// Inclusive supported limit.
        limit: u64,
    },

    /// Contextual malformed-data diagnostic requiring a dynamic explanation.
    #[error("malformed {context}: {detail}")]
    Malformed {
        /// Data structure or input being parsed.
        context: &'static str,
        /// Detailed validation failure.
        detail: Box<str>,
    },

    #[cfg(feature = "convert")]
    /// The `object` crate rejected an ELF input.
    #[error("failed to parse {input}: {source}")]
    Object {
        /// Role of the rejected ELF input.
        input: ElfInputKind,
        #[source]
        /// Underlying parser error.
        source: object::Error,
    },

    #[cfg(feature = "convert")]
    /// Gimli rejected malformed DWARF data.
    #[error("malformed DWARF: {source}")]
    Dwarf {
        #[source]
        /// Underlying DWARF parser error.
        source: gimli::Error,
    },

    #[cfg(feature = "convert")]
    /// A conversion input is a supported object format other than ELF.
    #[error("{input} is not an ELF file")]
    NotElf {
        /// Role of the non-ELF input.
        input: ElfInputKind,
    },

    #[cfg(feature = "convert")]
    /// A separate debug, symbol, or package file does not match the image.
    #[error("{input} {mismatch} does not match the linked image")]
    CompanionMismatch {
        /// Role of the mismatching companion.
        input: ElfInputKind,
        /// Identity property that differs.
        mismatch: CompanionMismatch,
    },

    /// A filesystem operation failed at a known path.
    #[error("failed to {operation} {}", path.display())]
    IoAtPath {
        /// Human-readable operation in progress.
        operation: &'static str,
        /// Filesystem path involved in the operation.
        path: std::path::PathBuf,
        #[source]
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A 64-bit semantic model cannot be narrowed into GSYM v1 fields.
    #[error("GSYM v1 limit exceeded for {field}: {value}; write version 2 explicitly")]
    V1LimitExceeded {
        /// Field that cannot be represented.
        field: &'static str,
        /// Value requiring more than 32 bits.
        value: u64,
    },

    /// An unscoped I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Crate-wide result type using [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn malformed(context: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self::Malformed {
            context,
            detail: detail.into(),
        }
    }
}

#[cfg(feature = "convert")]
impl From<gimli::Error> for Error {
    fn from(error: gimli::Error) -> Self {
        Self::Dwarf { source: error }
    }
}

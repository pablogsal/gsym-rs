use std::fmt;

use crate::AddressRange;

/// Non-fatal issue observed while importing ELF or DWARF data.
///
/// A warning means some input was skipped and conversion continued, so a report
/// carrying warnings is still usable. Anything that makes the whole conversion
/// impossible is an [`Error`](crate::Error) instead.
///
/// All variants implement `Display`, so `{warning}` is enough for a log line.
/// Matching specific variants needs a fallback arm.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConversionWarning {
    /// Embedded mini-debug data was present but unusable.
    EmbeddedDebugData {
        /// Rejection reason.
        reason: Box<str>,
    },
    /// A debuginfod request or cache operation failed.
    Debuginfod {
        /// Server or cache endpoint involved, when known.
        endpoint: Option<Box<str>>,
        /// Failure description.
        reason: Box<str>,
    },
    /// Neither an individual DWO nor a packaged DWP supplied a skeleton unit.
    SplitDwarfUnavailable {
        /// Split-DWARF unit identifier.
        dwo_id: u64,
        /// Failures encountered while trying each source.
        reasons: Box<[Box<str>]>,
    },
    /// Inline import was enabled but produced no valid inline records.
    NoInlineRecords,
    /// A DWARF range list contained malformed data.
    MalformedRanges {
        /// Whether parsing had to stop rather than skip one range.
        stopped: bool,
        /// Parser diagnostic.
        reason: Box<str>,
    },
    /// A DWARF range was invalid or outside live executable code.
    RejectedRange {
        /// Rejected virtual-address range.
        range: AddressRange,
    },
    /// `DW_AT_LLVM_stmt_sequence` did not identify a valid line sequence.
    InvalidStatementSequence {
        /// Absolute debug-info offset of the function DIE.
        die_offset: u64,
        /// Referenced line-program offset.
        sequence_offset: u64,
    },
    /// Executed line sequences could not be paired with scanned offsets.
    LineSequenceMismatch {
        /// Number of executed line sequences.
        sequences: usize,
        /// Number of scanned sequence offsets.
        offsets: usize,
    },
    /// A line row referenced a missing file-table entry.
    MissingLineFile {
        /// Row address.
        address: u64,
        /// Missing DWARF file index.
        index: u64,
    },
    /// A line number could not fit the GSYM `u32` field.
    UnrepresentableLine {
        /// Row address.
        address: u64,
        /// Original DWARF line number.
        line: u64,
    },
    /// An inline DIE referenced a missing call-site file.
    MissingInlineCallFile {
        /// Absolute debug-info offset of the inline DIE.
        die_offset: u64,
        /// Missing DWARF file index.
        index: u64,
    },
    /// An inline call line could not fit the GSYM `u32` field.
    InvalidInlineCallLine {
        /// Absolute debug-info offset of the inline DIE.
        die_offset: u64,
        /// Original DWARF line number.
        line: u64,
    },
    /// A declaration location referenced a missing file.
    InvalidDeclarationFile {
        /// Absolute debug-info offset of the function DIE.
        die_offset: u64,
        /// Missing DWARF file index.
        index: u64,
    },
    /// A declaration line could not fit the GSYM `u32` field.
    InvalidDeclarationLine {
        /// Absolute debug-info offset of the function DIE.
        die_offset: u64,
        /// Original DWARF line number.
        line: u64,
    },
}

impl fmt::Display for ConversionWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmbeddedDebugData { reason } => {
                write!(formatter, "ignoring malformed .gnu_debugdata: {reason}")
            }
            Self::Debuginfod { endpoint, reason } => {
                formatter.write_str("debuginfod")?;
                if let Some(endpoint) = endpoint {
                    write!(formatter, " {endpoint}")?;
                }
                write!(formatter, ": {reason}")
            }
            Self::SplitDwarfUnavailable { dwo_id, reasons } => {
                write!(
                    formatter,
                    "split DWARF unit {dwo_id:#x} has no usable .dwo or .dwp data"
                )?;
                for (index, reason) in reasons.iter().enumerate() {
                    formatter.write_str(if index == 0 { ": " } else { "; " })?;
                    formatter.write_str(reason)?;
                }
                Ok(())
            }
            Self::NoInlineRecords => {
                formatter.write_str("no valid DWARF inline records were found")
            }
            Self::MalformedRanges { stopped, reason } => write!(
                formatter,
                "{} malformed DWARF ranges: {reason}",
                if *stopped { "stopped at" } else { "skipping" }
            ),
            Self::RejectedRange { range } => write!(
                formatter,
                "skipping non-live or invalid DWARF range {:#x}..{:#x}",
                range.start, range.end
            ),
            Self::InvalidStatementSequence {
                die_offset,
                sequence_offset,
            } => write!(
                formatter,
                "function DIE at {die_offset:#x} has an invalid DW_AT_LLVM_stmt_sequence value {sequence_offset:#x}; using matching rows from other sequences"
            ),
            Self::LineSequenceMismatch { sequences, offsets } => write!(
                formatter,
                "could not associate {sequences} line sequences with {offsets} statement-sequence offsets"
            ),
            Self::MissingLineFile { address, index } => write!(
                formatter,
                "ignoring DWARF line row at {address:#x} with missing file index {index}"
            ),
            Self::UnrepresentableLine { address, line } => write!(
                formatter,
                "ignoring DWARF line row at {address:#x} with unrepresentable line number {line}"
            ),
            Self::MissingInlineCallFile { die_offset, index } => write!(
                formatter,
                "inline DIE at {die_offset:#x} references missing DW_AT_call_file index {index}"
            ),
            Self::InvalidInlineCallLine { die_offset, line } => write!(
                formatter,
                "inline DIE at {die_offset:#x} has unrepresentable DW_AT_call_line value {line}"
            ),
            Self::InvalidDeclarationFile { die_offset, index } => write!(
                formatter,
                "function DIE at {die_offset:#x} has invalid DW_AT_decl_file index {index}"
            ),
            Self::InvalidDeclarationLine { die_offset, line } => write!(
                formatter,
                "function DIE at {die_offset:#x} has unrepresentable DW_AT_decl_line value {line}"
            ),
        }
    }
}

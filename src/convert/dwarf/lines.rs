use std::collections::HashMap;

use gimli::{AttributeValue, Format, Reader};

use super::references::attribute_bytes;
use super::{DW_AT_LLVM_STMT_SEQUENCE, gimli_error};
use crate::convert::ConversionWarning;
use crate::model::{AddressRange, FileEntry, FileIndex, LineEntry};
use crate::normalize::compact_line_rows;
use crate::{Error, GsymBuilder, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SequencedLine {
    pub(super) entry: LineEntry,
    pub(super) statement_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LineSequenceRange {
    pub(super) range: AddressRange,
    pub(super) statement_sequence: Option<u64>,
}

pub(super) struct UnitLines {
    pub(super) entries: Vec<SequencedLine>,
    pub(super) files: HashMap<u64, FileIndex>,
    pub(super) sequences: Vec<LineSequenceRange>,
}

impl UnitLines {
    pub(super) fn for_range(
        &self,
        range: AddressRange,
        requested_sequence: Option<u64>,
    ) -> (Vec<LineEntry>, bool) {
        let sequence_exists = requested_sequence.is_none_or(|requested| {
            self.sequences
                .iter()
                .any(|sequence| sequence.statement_sequence == Some(requested))
        });
        let selected_sequence = requested_sequence.filter(|_| sequence_exists);
        let clamping_sequence = selected_sequence.or_else(|| {
            self.sequences
                .iter()
                .find(|sequence| sequence.range.contains(range.start))
                .and_then(|sequence| sequence.statement_sequence)
        });

        let start = self
            .entries
            .partition_point(|line| line.entry.address < range.start);
        let end = self
            .entries
            .partition_point(|line| line.entry.address < range.end);
        let mut output = self
            .entries
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .filter(|line| {
                selected_sequence.is_none_or(|selected| line.statement_sequence == Some(selected))
            })
            .map(|line| line.entry)
            .collect::<Vec<_>>();

        if self.sequences.iter().any(|sequence| {
            selected_sequence.is_none_or(|selected| sequence.statement_sequence == Some(selected))
                && sequence.range.contains(range.start)
        }) && output.first().is_none_or(|line| line.address > range.start)
            && let Some(previous) = self
                .entries
                .get(..start)
                .unwrap_or_default()
                .iter()
                .rev()
                .find(|line| {
                    clamping_sequence
                        .is_none_or(|selected| line.statement_sequence == Some(selected))
                })
        {
            let mut clamped = previous.entry;
            clamped.address = range.start;
            output.insert(0, clamped);
        }
        compact_line_rows(&mut output);
        let invalid_sequence =
            requested_sequence.is_some() && !sequence_exists && !output.is_empty();
        (output, invalid_sequence)
    }
}

pub(super) fn collect_lines<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &gimli::Unit<R>,
    builder: &mut GsymBuilder,
    warnings: &mut Vec<ConversionWarning>,
) -> Result<UnitLines> {
    let Some(line_program) = unit.line_program.clone() else {
        return Ok(UnitLines {
            entries: Vec::new(),
            files: HashMap::new(),
            sequences: Vec::new(),
        });
    };
    let (program, sequences) = line_program.sequences().map_err(gimli_error)?;
    let header = program.header();
    let statement_sequence_offsets = line_sequence_offsets(header)?;
    if statement_sequence_offsets.len() != sequences.len() {
        warnings.push(ConversionWarning::LineSequenceMismatch {
            sequences: sequences.len(),
            offsets: statement_sequence_offsets.len(),
        });
    }
    let mut files = intern_header_files(dwarf, unit, header, builder)?;
    let mut output = Vec::new();
    let mut sequence_ranges = Vec::new();
    for (index, sequence) in sequences.into_iter().enumerate() {
        let statement_sequence = statement_sequence_offsets.get(index).copied();
        if sequence.start < sequence.end {
            sequence_ranges.push(LineSequenceRange {
                range: AddressRange::new(sequence.start, sequence.end),
                statement_sequence,
            });
        }
        let mut rows = program.resume_from(&sequence);
        while let Some((header, row)) = rows.next_row().map_err(gimli_error)? {
            if row.end_sequence() {
                continue;
            }
            let dwarf_index = row.file_index();
            let file = if let Some(file) = files.get(&dwarf_index).copied() {
                file
            } else {
                let Some(entry) = row.file(header) else {
                    warnings.push(ConversionWarning::MissingLineFile {
                        address: row.address(),
                        index: dwarf_index,
                    });
                    continue;
                };
                let Some(file) = intern_file(dwarf, unit, header, entry, builder)? else {
                    continue;
                };
                files.insert(dwarf_index, file);
                file
            };
            let line = match row.line() {
                None => 0,
                Some(line) => {
                    if let Ok(line) = u32::try_from(line.get()) {
                        line
                    } else {
                        warnings.push(ConversionWarning::UnrepresentableLine {
                            address: row.address(),
                            line: line.get(),
                        });
                        continue;
                    }
                }
            };
            output.push(SequencedLine {
                entry: LineEntry {
                    address: row.address(),
                    file,
                    line,
                },
                statement_sequence,
            });
        }
    }
    output.sort_by_key(|row| row.entry.address);
    output.dedup();
    Ok(UnitLines {
        entries: output,
        files,
        sequences: sequence_ranges,
    })
}

pub(super) fn statement_sequence_offset<R: Reader<Offset = usize>>(
    unit: &gimli::Unit<R>,
    entry: &gimli::DebuggingInformationEntry<R>,
) -> Option<u64> {
    let value = match entry.attr_value(DW_AT_LLVM_STMT_SEQUENCE) {
        Some(AttributeValue::SecOffset(offset)) => Some(offset as u64),
        Some(AttributeValue::DebugLineRef(offset)) => Some(offset.0 as u64),
        Some(value) => value.udata_value(),
        None => None,
    }?;
    let invalid = match unit.encoding().format {
        Format::Dwarf32 => u64::from(u32::MAX),
        Format::Dwarf64 => u64::MAX,
    };
    (value != invalid).then_some(value)
}

fn line_sequence_offsets<R: Reader<Offset = usize>>(
    header: &gimli::LineProgramHeader<R>,
) -> Result<Vec<u64>> {
    let program = header.raw_program_buf();
    let program = program.to_slice().map_err(gimli_error)?;
    let standard_opcode_lengths = header
        .standard_opcode_lengths()
        .to_slice()
        .map_err(gimli_error)?;
    let encoding = header.encoding();
    let program_offset = (header.offset().0 as u64)
        .checked_add(u64::from(encoding.format.initial_length_size()))
        .and_then(|offset| offset.checked_add(2))
        .and_then(|offset| offset.checked_add(u64::from(encoding.version >= 5).saturating_mul(2)))
        .and_then(|offset| offset.checked_add(u64::from(encoding.format.word_size())))
        .and_then(|offset| offset.checked_add(header.header_length() as u64))
        .ok_or(Error::Overflow("DWARF line-program offset"))?;
    scan_line_sequence_offsets(
        program.as_ref(),
        standard_opcode_lengths.as_ref(),
        header.opcode_base(),
        program_offset,
    )
}

/// Records the `.debug_line` offset at which each sequence's instructions begin.
///
/// `DW_AT_LLVM_stmt_sequence` points at one of these offsets, so a subprogram
/// can select the single line sequence that belongs to it. Gimli runs the line
/// program but does not report where each sequence started: `LineSequence`
/// keeps its `instructions` private, as does `LineInstruction::parse`, leaving
/// no public way to observe the reader offset mid-iteration.
///
/// So this walks the opcode stream itself, which means it carries a second copy
/// of gimli's operand-length rules. Keep the two in step when upgrading gimli,
/// and delete this once gimli exposes a per-sequence program offset.
pub(super) fn scan_line_sequence_offsets(
    program: &[u8],
    standard_opcode_lengths: &[u8],
    opcode_base: u8,
    program_offset: u64,
) -> Result<Vec<u64>> {
    let mut cursor = 0_usize;
    let mut sequence_start = program_offset;
    let mut output = Vec::new();
    while cursor < program.len() {
        let opcode = read_line_byte(program, &mut cursor)?;
        if opcode == 0 {
            let length = usize::try_from(read_line_uleb(program, &mut cursor)?)
                .map_err(|_| Error::Overflow("extended DWARF line opcode length"))?;
            if length == 0 {
                return Err(Error::malformed(
                    "DWARF line program",
                    "zero-length extended opcode",
                ));
            }
            let payload_start = cursor;
            let subopcode = read_line_byte(program, &mut cursor)
                .map_err(|_| Error::malformed("DWARF line program", "truncated extended opcode"))?;
            take_line_bytes(program, &mut cursor, length.saturating_sub(1))?;
            if subopcode == gimli::constants::DW_LNE_end_sequence.0 {
                output.push(sequence_start);
                sequence_start = program_offset
                    .checked_add(cursor as u64)
                    .ok_or(Error::Overflow("DWARF line sequence offset"))?;
            }
            debug_assert_eq!(cursor, payload_start.saturating_add(length));
        } else if opcode < opcode_base {
            match gimli::DwLns(opcode) {
                gimli::constants::DW_LNS_fixed_advance_pc => {
                    take_line_bytes(program, &mut cursor, 2)?;
                }
                gimli::constants::DW_LNS_copy
                | gimli::constants::DW_LNS_negate_stmt
                | gimli::constants::DW_LNS_set_basic_block
                | gimli::constants::DW_LNS_const_add_pc
                | gimli::constants::DW_LNS_set_prologue_end
                | gimli::constants::DW_LNS_set_epilogue_begin => {}
                _ => {
                    let operand_count = standard_opcode_lengths
                        .get(usize::from(opcode.saturating_sub(1)))
                        .copied()
                        .unwrap_or(0);
                    for _ in 0..operand_count {
                        let _ = read_line_uleb(program, &mut cursor)?;
                    }
                }
            }
        }
    }
    Ok(output)
}

fn read_line_byte(program: &[u8], cursor: &mut usize) -> Result<u8> {
    let byte = program
        .get(*cursor)
        .copied()
        .ok_or_else(|| Error::malformed("DWARF line program", "truncated instruction"))?;
    *cursor = cursor.saturating_add(1);
    Ok(byte)
}

fn take_line_bytes(program: &[u8], cursor: &mut usize, length: usize) -> Result<()> {
    let end = cursor
        .checked_add(length)
        .ok_or(Error::Overflow("DWARF line instruction length"))?;
    program
        .get(*cursor..end)
        .ok_or_else(|| Error::malformed("DWARF line program", "truncated instruction"))?;
    *cursor = end;
    Ok(())
}

fn read_line_uleb(program: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0_u64;
    for shift in (0..=63).step_by(7) {
        let byte = read_line_byte(program, cursor)?;
        let payload = u64::from(byte & 0x7f);
        if shift == 63 && payload > 1 {
            return Err(Error::malformed(
                "DWARF line program",
                "overflowing ULEB128 operand",
            ));
        }
        value |= payload << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Error::malformed(
        "DWARF line program",
        "overlong ULEB128 operand",
    ))
}

pub(super) fn intern_header_files<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &gimli::Unit<R>,
    header: &gimli::LineProgramHeader<R>,
    builder: &mut GsymBuilder,
) -> Result<HashMap<u64, FileIndex>> {
    let first_file_index = u64::from(header.version() <= 4);
    let mut files = HashMap::with_capacity(header.file_names().len());
    for (offset, file) in header.file_names().iter().enumerate() {
        let dwarf_index = first_file_index
            .checked_add(
                u64::try_from(offset).map_err(|_| Error::Overflow("DWARF file-table index"))?,
            )
            .ok_or(Error::Overflow("DWARF file-table index"))?;
        if let Some(gsym_index) = intern_file(dwarf, unit, header, file, builder)? {
            files.insert(dwarf_index, gsym_index);
        }
    }
    Ok(files)
}

fn intern_file<R: Reader<Offset = usize>>(
    dwarf: &gimli::Dwarf<R>,
    unit: &gimli::Unit<R>,
    header: &gimli::LineProgramHeader<R>,
    file: &gimli::FileEntry<R>,
    builder: &mut GsymBuilder,
) -> Result<Option<FileIndex>> {
    let basename = attribute_bytes(dwarf, unit, file.path_name())?;
    if basename.is_empty() {
        return Ok(None);
    }
    let directory = match file.directory(header) {
        Some(value) => attribute_bytes(dwarf, unit, value)?,
        None => Vec::new(),
    };
    builder
        .add_file(FileEntry {
            directory,
            basename,
        })
        .map(Some)
}

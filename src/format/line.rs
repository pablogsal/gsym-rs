use std::num::NonZeroU64;

use crate::endian::{Cursor, Encoder, Endian};
use crate::error::{Error, Result};
use crate::model::{FileIndex, LineEntry};

use super::leb::{read_sleb, read_uleb, write_sleb, write_uleb};

const END_SEQUENCE: u8 = 0;
const SET_FILE: u8 = 1;
const ADVANCE_PC: u8 = 2;
const ADVANCE_LINE: u8 = 3;
const FIRST_SPECIAL: u8 = 4;

pub(crate) fn decode(bytes: &[u8], endian: Endian, base: u64) -> Result<Vec<LineEntry>> {
    let mut cursor = Cursor::new(bytes, endian);
    let mut lines = Vec::new();
    parse(&mut cursor, base, |row| {
        lines.push(row);
        true
    })?;
    Ok(lines)
}

pub(crate) fn lookup(
    bytes: &[u8],
    endian: Endian,
    base: u64,
    address: u64,
) -> Result<Option<LineEntry>> {
    let mut cursor = Cursor::new(bytes, endian);
    let mut result = None;
    parse(&mut cursor, base, |row| {
        if address < row.address {
            false
        } else {
            result = Some(row);
            true
        }
    })?;
    Ok(result)
}

fn parse(
    cursor: &mut Cursor<'_>,
    base: u64,
    mut row_callback: impl FnMut(LineEntry) -> bool,
) -> Result<()> {
    let minimum_delta = read_sleb(cursor)?;
    let maximum_delta = read_sleb(cursor)?;
    let line_range = special_line_range(minimum_delta, maximum_delta)?;
    let first_line = read_uleb(cursor)?;
    let mut row = LineEntry {
        address: base,
        file: FileIndex::new(1),
        line: u32::try_from(first_line).map_err(|_| Error::OutOfRange {
            field: "line number",
            value: first_line,
            max: u64::from(u32::MAX),
        })?,
    };

    loop {
        let opcode = cursor.read_u8().map_err(|error| {
            if matches!(error, Error::UnexpectedEof { .. }) {
                Error::InvalidFormat("line table has no end-sequence opcode")
            } else {
                error
            }
        })?;
        match opcode {
            END_SEQUENCE => return Ok(()),
            SET_FILE => {
                let file = read_uleb(cursor)?;
                row.file = FileIndex::new(u32::try_from(file).map_err(|_| Error::OutOfRange {
                    field: "line-table file index",
                    value: file,
                    max: u64::from(u32::MAX),
                })?);
            }
            ADVANCE_PC => {
                row.address = row
                    .address
                    .checked_add(read_uleb(cursor)?)
                    .ok_or(Error::Overflow("line-table address"))?;
                if !row_callback(row) {
                    return Ok(());
                }
            }
            ADVANCE_LINE => {
                row.line = add_line_delta(row.line, read_sleb(cursor)?)?;
            }
            special => {
                let adjusted = u64::from(special.saturating_sub(FIRST_SPECIAL));
                let line_delta = minimum_delta
                    .checked_add_unsigned(adjusted % line_range)
                    .ok_or(Error::Overflow("line-table line delta"))?;
                let address_delta = adjusted / line_range;
                row.line = add_line_delta(row.line, line_delta)?;
                row.address = row
                    .address
                    .checked_add(address_delta)
                    .ok_or(Error::Overflow("line-table address"))?;
                if !row_callback(row) {
                    return Ok(());
                }
            }
        }
    }
}

fn special_line_range(minimum: i64, maximum: i64) -> Result<NonZeroU64> {
    if maximum < minimum {
        return Err(Error::InvalidFormat(
            "line-table maximum delta precedes minimum delta",
        ));
    }
    let span = maximum
        .checked_sub(minimum)
        .ok_or(Error::Overflow("line-table delta range"))?;
    Ok(NonZeroU64::MIN.saturating_add(span.unsigned_abs()))
}

fn add_line_delta(line: u32, delta: i64) -> Result<u32> {
    let value = i128::from(line).saturating_add(i128::from(delta));
    u32::try_from(value).map_err(|_| {
        Error::malformed(
            "line table",
            format!("line calculation is outside u32: {line} + {delta}"),
        )
    })
}

#[cfg(test)]
pub(crate) fn encode(lines: &[LineEntry], endian: Endian, base: u64) -> Result<Vec<u8>> {
    let mut output = Encoder::new(endian);
    encode_into(lines, &mut output, base)?;
    Ok(output.into_inner())
}

pub(crate) fn encode_into(lines: &[LineEntry], output: &mut Encoder, base: u64) -> Result<()> {
    if lines.is_empty() {
        return Err(Error::InvalidModel("line table must not be empty"));
    }

    let first = lines
        .first()
        .ok_or(Error::InvalidModel("line table must not be empty"))?;
    let (minimum_delta, maximum_delta) = choose_delta_range(lines);
    write_sleb(output, minimum_delta);
    write_sleb(output, maximum_delta);
    write_uleb(output, u64::from(first.line));

    let mut previous = LineEntry {
        address: base,
        file: FileIndex::new(1),
        line: first.line,
    };
    for current in lines {
        if current.address < base {
            return Err(Error::InvalidModel(
                "line address precedes the function start",
            ));
        }
        if current.address < previous.address {
            return Err(Error::InvalidModel(
                "line-table addresses are not monotonically increasing",
            ));
        }

        let address_delta = current.address.saturating_sub(previous.address);
        let line_delta = i64::from(current.line).saturating_sub(i64::from(previous.line));
        if current.file != previous.file {
            output.write_u8(SET_FILE);
            write_uleb(output, u64::from(current.file));
        }

        if let Some(special) =
            encode_special(minimum_delta, maximum_delta, line_delta, address_delta)
        {
            output.write_u8(special);
        } else {
            if line_delta != 0 {
                output.write_u8(ADVANCE_LINE);
                write_sleb(output, line_delta);
            }
            output.write_u8(ADVANCE_PC);
            write_uleb(output, address_delta);
        }
        previous = *current;
    }
    output.write_u8(END_SEQUENCE);
    Ok(())
}

fn choose_delta_range(lines: &[LineEntry]) -> (i64, i64) {
    const MAXIMUM_LINE_RANGE: i64 = 14;

    if lines.len() < 2 {
        return (0, 0);
    }

    let deltas = lines
        .iter()
        .zip(lines.iter().skip(1))
        .map(|(previous, current)| {
            i64::from(current.line).saturating_sub(i64::from(previous.line))
        });
    let (mut minimum, mut maximum) = (i64::MAX, i64::MIN);
    for delta in deltas.clone() {
        minimum = minimum.min(delta);
        maximum = maximum.max(delta);
        if maximum.saturating_sub(minimum) > MAXIMUM_LINE_RANGE {
            break;
        }
    }
    if maximum.saturating_sub(minimum) > MAXIMUM_LINE_RANGE {
        let mut deltas: Vec<_> = deltas.collect();
        deltas.sort_unstable();
        let mut best_count = 0_u32;
        let mut end = 0;
        for (start, &first) in deltas.iter().enumerate() {
            while deltas
                .get(end)
                .is_some_and(|delta| delta.saturating_sub(first) <= MAXIMUM_LINE_RANGE)
            {
                end = end.saturating_add(1);
            }
            let count = u32::try_from(end.saturating_sub(start)).unwrap_or(u32::MAX);
            if count > best_count {
                best_count = count;
                minimum = first;
                maximum = deltas.get(end.saturating_sub(1)).copied().unwrap_or(first);
            }
        }
    }
    if minimum == maximum && minimum > 0 && minimum < MAXIMUM_LINE_RANGE {
        minimum = 0;
    }
    (minimum, maximum)
}

fn encode_special(
    minimum_delta: i64,
    maximum_delta: i64,
    line_delta: i64,
    address_delta: u64,
) -> Option<u8> {
    if line_delta < minimum_delta || line_delta > maximum_delta {
        return None;
    }
    let line_range = maximum_delta.checked_sub(minimum_delta)?.checked_add(1)?;
    let adjusted = i128::from(line_delta.saturating_sub(minimum_delta))
        .saturating_add(i128::from(address_delta).saturating_mul(i128::from(line_range)));
    let opcode = adjusted.saturating_add(i128::from(FIRST_SPECIAL));
    u8::try_from(opcode).ok()
}

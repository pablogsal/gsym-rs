use crate::endian::{Cursor, Encoder, Endian};
use crate::error::{Error, Result};
use crate::format::{INFO_CALL_SITE, INFO_END, INFO_INLINE, INFO_LINE_TABLE, INFO_MERGED};
use crate::model::{AddressRange, LineEntry};

use super::leb::{read_uleb, write_uleb};
use super::line;

const MAX_INLINE_DEPTH: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InfoType {
    LineTable,
    Inline,
    Merged,
    CallSite,
    Unknown(u32),
}

impl InfoType {
    const fn from_raw(value: u32) -> Self {
        match value {
            INFO_LINE_TABLE => Self::LineTable,
            INFO_INLINE => Self::Inline,
            INFO_MERGED => Self::Merged,
            INFO_CALL_SITE => Self::CallSite,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InfoRecord<'data> {
    pub(crate) kind: InfoType,
    pub(crate) payload: &'data [u8],
}

#[derive(Default)]
pub(crate) struct RecordSet {
    bits: u8,
}

impl RecordSet {
    pub(crate) const fn observe(&mut self, kind: InfoType) -> Result<()> {
        let (bit, message) = match kind {
            InfoType::LineTable => (1 << 0, "duplicate line-table record"),
            InfoType::Inline => (1 << 1, "duplicate inline-info record"),
            InfoType::Merged => (1 << 2, "duplicate merged-functions record"),
            InfoType::CallSite => (1 << 3, "duplicate call-site record"),
            InfoType::Unknown(_) => return Ok(()),
        };
        if self.bits & bit != 0 {
            Err(Error::InvalidFormat(message))
        } else {
            self.bits |= bit;
            Ok(())
        }
    }
}

pub(crate) fn next_record<'data>(
    cursor: &mut Cursor<'data>,
    ended: &mut bool,
) -> Result<Option<InfoRecord<'data>>> {
    if *ended {
        return Ok(None);
    }
    let raw_kind = cursor.read_u32()?;
    let length = usize::try_from(cursor.read_u32()?)
        .map_err(|_| Error::Overflow("FunctionInfo record length"))?;
    if raw_kind == INFO_END {
        *ended = true;
        if length != 0 {
            return Err(Error::InvalidFormat(
                "FunctionInfo end marker has a nonzero length",
            ));
        }
        return Ok(None);
    }
    Ok(Some(InfoRecord {
        kind: InfoType::from_raw(raw_kind),
        payload: cursor.take(length)?,
    }))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EncodedInlineNode {
    pub(crate) ranges: Vec<AddressRange>,
    pub(crate) name: u64,
    pub(crate) call_file: u32,
    pub(crate) call_line: u32,
    pub(crate) children: Vec<Self>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EncodedCallSite {
    pub(crate) return_offset: u64,
    pub(crate) flags: u8,
    pub(crate) match_regex: Vec<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EncodedFunction {
    pub(crate) range: AddressRange,
    pub(crate) name: u64,
    pub(crate) lines: Option<Vec<LineEntry>>,
    pub(crate) inline: Option<EncodedInlineNode>,
    pub(crate) merged: Vec<Self>,
    pub(crate) call_sites: Vec<EncodedCallSite>,
}

pub(crate) fn decode(
    bytes: &[u8],
    endian: Endian,
    string_offset_size: u8,
    base: u64,
) -> Result<EncodedFunction> {
    validate_string_offset_size(string_offset_size)?;
    let mut cursor = Cursor::new(bytes, endian);
    decode_from(&mut cursor, string_offset_size, base)
}

pub(crate) fn decode_exact(
    bytes: &[u8],
    endian: Endian,
    string_offset_size: u8,
    base: u64,
) -> Result<EncodedFunction> {
    validate_string_offset_size(string_offset_size)?;
    let mut cursor = Cursor::new(bytes, endian);
    let function = decode_from(&mut cursor, string_offset_size, base)?;
    if !cursor.is_empty() {
        return Err(Error::InvalidFormat(
            "bytes remain after the FunctionInfo end marker",
        ));
    }
    Ok(function)
}

pub(crate) fn decode_from(
    cursor: &mut Cursor<'_>,
    string_offset_size: u8,
    base: u64,
) -> Result<EncodedFunction> {
    validate_string_offset_size(string_offset_size)?;
    let size = u64::from(cursor.read_u32()?);
    let name = cursor.read_uint(string_offset_size)?;
    if name == 0 {
        return Err(Error::ZeroNameOffset);
    }
    let end = base
        .checked_add(size)
        .ok_or(Error::Overflow("FunctionInfo range end"))?;
    let mut function = EncodedFunction {
        range: AddressRange::new(base, end),
        name,
        ..EncodedFunction::default()
    };
    let mut seen = RecordSet::default();
    let mut ended = false;

    while let Some(record) = next_record(cursor, &mut ended)? {
        seen.observe(record.kind)?;
        match record.kind {
            InfoType::LineTable => {
                function.lines = Some(line::decode(record.payload, cursor.endian(), base)?);
            }
            InfoType::Inline => {
                let mut data = Cursor::new(record.payload, cursor.endian());
                let inline = decode_inline(&mut data, string_offset_size, base, 0)?;
                if inline.ranges.is_empty() {
                    return Err(Error::InvalidFormat(
                        "top-level inline info has no address ranges",
                    ));
                }
                if !data.is_empty() {
                    return Err(Error::InvalidFormat("trailing inline-info bytes"));
                }
                function.inline = Some(inline);
            }
            InfoType::Merged => {
                function.merged =
                    decode_merged(record.payload, cursor.endian(), string_offset_size, base)?;
            }
            InfoType::CallSite => {
                function.call_sites =
                    decode_call_sites(record.payload, cursor.endian(), string_offset_size)?;
            }
            InfoType::Unknown(other) => return Err(Error::UnsupportedInfoType(other)),
        }
    }
    Ok(function)
}

#[cfg(test)]
pub(crate) fn encode(
    function: &EncodedFunction,
    endian: Endian,
    string_offset_size: u8,
) -> Result<Vec<u8>> {
    let mut output = Encoder::new(endian);
    encode_into(function, &mut output, string_offset_size, false)?;
    Ok(output.into_inner())
}

pub(crate) fn encode_into(
    function: &EncodedFunction,
    output: &mut Encoder,
    string_offset_size: u8,
    align: bool,
) -> Result<usize> {
    validate_string_offset_size(string_offset_size)?;
    if align {
        output.align_to(4)?;
    }
    let start = output.len();
    if function.name == 0 {
        return Err(Error::ZeroNameOffset);
    }
    if function.range.end < function.range.start {
        return Err(Error::InvalidModel("function range end precedes its start"));
    }
    let size = function.range.end.saturating_sub(function.range.start);
    output.write_u32(u32::try_from(size).map_err(|_| Error::Limit {
        context: "FunctionInfo size",
        value: size,
        limit: u64::from(u32::MAX),
    })?);
    output.write_uint(function.name, string_offset_size)?;

    if let Some(lines) = &function.lines {
        write_record(output, INFO_LINE_TABLE, |output| {
            line::encode_into(lines, output, function.range.start)
        })?;
    }
    if let Some(inline) = &function.inline {
        write_record(output, INFO_INLINE, |output| {
            encode_inline(inline, output, string_offset_size, function.range.start, 0)
        })?;
    }
    if !function.merged.is_empty() {
        write_record(output, INFO_MERGED, |output| {
            encode_merged_into(
                &function.merged,
                output,
                string_offset_size,
                function.range.start,
            )
        })?;
    }
    if !function.call_sites.is_empty() {
        write_record(output, INFO_CALL_SITE, |output| {
            encode_call_sites_into(&function.call_sites, output, string_offset_size)
        })?;
    }
    output.write_u32(INFO_END);
    output.write_u32(0);
    Ok(start)
}

fn write_record(
    output: &mut Encoder,
    info_type: u32,
    encode_payload: impl FnOnce(&mut Encoder) -> Result<()>,
) -> Result<()> {
    output.write_u32(info_type);
    let length_offset = output.len();
    output.write_u32(0);
    let payload_offset = output.len();
    encode_payload(output)?;
    let payload_len = output
        .len()
        .checked_sub(payload_offset)
        .ok_or(Error::Overflow("FunctionInfo record length"))?;
    let payload_len = u32::try_from(payload_len).map_err(|_| Error::Limit {
        context: "FunctionInfo record",
        value: payload_len as u64,
        limit: u64::from(u32::MAX),
    })?;
    output.patch_u32(length_offset, payload_len)
}

fn validate_string_offset_size(size: u8) -> Result<()> {
    if matches!(size, 1 | 2 | 4 | 8) {
        Ok(())
    } else {
        Err(Error::OutOfRange {
            field: "string offset size",
            value: u64::from(size),
            max: 8,
        })
    }
}

fn decode_ranges(cursor: &mut Cursor<'_>, base: u64) -> Result<Vec<AddressRange>> {
    let count = read_uleb(cursor)?;
    let count = usize::try_from(count).map_err(|_| Error::Overflow("address-range count"))?;
    if count > cursor.remaining() / 2 {
        return Err(Error::InvalidFormat(
            "address-range count exceeds remaining input",
        ));
    }
    let mut ranges: Vec<AddressRange> = Vec::with_capacity(count);
    for _ in 0..count {
        let start = base
            .checked_add(read_uleb(cursor)?)
            .ok_or(Error::Overflow("relative address range start"))?;
        let end = start
            .checked_add(read_uleb(cursor)?)
            .ok_or(Error::Overflow("address range end"))?;
        let range = AddressRange::new(start, end);
        if let Some(previous) = ranges.last()
            && range.start < previous.end
        {
            return Err(Error::InvalidFormat(
                "address ranges overlap or are not sorted",
            ));
        }
        ranges.push(range);
    }
    Ok(ranges)
}

fn encode_ranges(ranges: &[AddressRange], output: &mut Encoder, base: u64) -> Result<()> {
    write_uleb(
        output,
        u64::try_from(ranges.len()).map_err(|_| Error::Overflow("address-range count"))?,
    );
    let mut previous_end = None;
    for range in ranges {
        if range.start < base || range.end < range.start {
            return Err(Error::InvalidModel("invalid relative address range"));
        }
        if previous_end.is_some_and(|end| range.start < end) {
            return Err(Error::InvalidModel(
                "address ranges overlap or are not sorted",
            ));
        }
        write_uleb(
            output,
            range.start.checked_sub(base).ok_or(Error::InvalidModel(
                "address range precedes the base address",
            ))?,
        );
        write_uleb(output, range.end.saturating_sub(range.start));
        previous_end = Some(range.end);
    }
    Ok(())
}

fn decode_inline(
    cursor: &mut Cursor<'_>,
    string_offset_size: u8,
    base: u64,
    depth: usize,
) -> Result<EncodedInlineNode> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::InvalidFormat("inline tree exceeds maximum depth"));
    }
    let ranges = decode_ranges(cursor, base)?;
    if ranges.is_empty() {
        return Ok(EncodedInlineNode::default());
    }
    let has_children = cursor.read_u8()? != 0;
    let name = cursor.read_uint(string_offset_size)?;
    let call_file = read_uleb(cursor)?;
    let call_line = read_uleb(cursor)?;
    let mut node = EncodedInlineNode {
        ranges,
        name,
        call_file: u32::try_from(call_file).map_err(|_| Error::OutOfRange {
            field: "inline call-file index",
            value: call_file,
            max: u64::from(u32::MAX),
        })?,
        call_line: u32::try_from(call_line).map_err(|_| Error::OutOfRange {
            field: "inline call-line",
            value: call_line,
            max: u64::from(u32::MAX),
        })?,
        children: Vec::new(),
    };
    if has_children {
        let child_base = node
            .ranges
            .first()
            .ok_or(Error::InvalidFormat(
                "inline node with children has no range",
            ))?
            .start;
        loop {
            let child = decode_inline(
                cursor,
                string_offset_size,
                child_base,
                depth.saturating_add(1),
            )?;
            if child.ranges.is_empty() {
                break;
            }
            if child.ranges.iter().any(|range| {
                !node
                    .ranges
                    .iter()
                    .any(|parent| parent.contains_range(*range))
            }) {
                return Err(Error::InvalidFormat(
                    "inline child range is outside its parent",
                ));
            }
            node.children.push(child);
        }
    }
    Ok(node)
}

fn encode_inline(
    node: &EncodedInlineNode,
    output: &mut Encoder,
    string_offset_size: u8,
    base: u64,
    depth: usize,
) -> Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::InvalidModel("inline tree exceeds maximum depth"));
    }
    if node.ranges.is_empty() {
        return Err(Error::InvalidModel("inline node must contain a range"));
    }
    encode_ranges(&node.ranges, output, base)?;
    output.write_u8(u8::from(!node.children.is_empty()));
    output.write_uint(node.name, string_offset_size)?;
    write_uleb(output, u64::from(node.call_file));
    write_uleb(output, u64::from(node.call_line));
    if !node.children.is_empty() {
        let child_base = node
            .ranges
            .first()
            .ok_or(Error::InvalidModel(
                "inline node with children has no range",
            ))?
            .start;
        for child in &node.children {
            if child.ranges.iter().any(|range| {
                !node
                    .ranges
                    .iter()
                    .any(|parent| parent.contains_range(*range))
            }) {
                return Err(Error::InvalidModel(
                    "inline child range is outside its parent",
                ));
            }
            encode_inline(
                child,
                output,
                string_offset_size,
                child_base,
                depth.saturating_add(1),
            )?;
        }
        write_uleb(output, 0);
    }
    Ok(())
}

fn decode_merged(
    bytes: &[u8],
    endian: Endian,
    string_offset_size: u8,
    base: u64,
) -> Result<Vec<EncodedFunction>> {
    let mut cursor = Cursor::new(bytes, endian);
    let count = usize::try_from(cursor.read_u32()?)
        .map_err(|_| Error::Overflow("merged-function count"))?;
    if count > cursor.remaining() / 4 {
        return Err(Error::InvalidFormat(
            "merged-function count exceeds remaining input",
        ));
    }
    let mut functions = Vec::with_capacity(count);
    for _ in 0..count {
        let length = usize::try_from(cursor.read_u32()?)
            .map_err(|_| Error::Overflow("merged FunctionInfo length"))?;
        let data = cursor.take(length)?;
        functions.push(decode_exact(data, endian, string_offset_size, base)?);
    }
    if !cursor.is_empty() {
        return Err(Error::InvalidFormat("trailing merged-function bytes"));
    }
    Ok(functions)
}

fn encode_merged_into(
    functions: &[EncodedFunction],
    output: &mut Encoder,
    string_offset_size: u8,
    base: u64,
) -> Result<()> {
    output.write_u32(u32::try_from(functions.len()).map_err(|_| Error::Limit {
        context: "merged-function count",
        value: functions.len() as u64,
        limit: u64::from(u32::MAX),
    })?);
    for function in functions {
        if function.range.start != base {
            return Err(Error::InvalidModel(
                "merged function start differs from its parent",
            ));
        }
        let length_offset = output.len();
        output.write_u32(0);
        let function_offset = output.len();
        encode_into(function, output, string_offset_size, false)?;
        let function_len = output
            .len()
            .checked_sub(function_offset)
            .ok_or(Error::Overflow("merged FunctionInfo length"))?;
        let function_len = u32::try_from(function_len).map_err(|_| Error::Limit {
            context: "merged FunctionInfo length",
            value: function_len as u64,
            limit: u64::from(u32::MAX),
        })?;
        output.patch_u32(length_offset, function_len)?;
    }
    Ok(())
}

fn decode_call_sites(
    bytes: &[u8],
    endian: Endian,
    string_offset_size: u8,
) -> Result<Vec<EncodedCallSite>> {
    let mut cursor = Cursor::new(bytes, endian);
    let count =
        usize::try_from(cursor.read_u32()?).map_err(|_| Error::Overflow("call-site count"))?;
    let minimum_record_size = 8 + 1 + 4;
    if count > cursor.remaining() / minimum_record_size {
        return Err(Error::InvalidFormat(
            "call-site count exceeds remaining input",
        ));
    }
    let mut call_sites = Vec::with_capacity(count);
    for _ in 0..count {
        let return_offset = cursor.read_u64()?;
        let flags = cursor.read_u8()?;
        let regex_count = usize::try_from(cursor.read_u32()?)
            .map_err(|_| Error::Overflow("call-site regex count"))?;
        if regex_count
            > cursor
                .remaining()
                .checked_div(usize::from(string_offset_size))
                .unwrap_or(0)
        {
            return Err(Error::InvalidFormat(
                "call-site regex count exceeds remaining input",
            ));
        }
        let mut match_regex = Vec::with_capacity(regex_count);
        for _ in 0..regex_count {
            match_regex.push(cursor.read_uint(string_offset_size)?);
        }
        call_sites.push(EncodedCallSite {
            return_offset,
            flags,
            match_regex,
        });
    }
    if !cursor.is_empty() {
        return Err(Error::InvalidFormat("trailing call-site bytes"));
    }
    Ok(call_sites)
}

fn encode_call_sites_into(
    call_sites: &[EncodedCallSite],
    output: &mut Encoder,
    string_offset_size: u8,
) -> Result<()> {
    output.write_u32(u32::try_from(call_sites.len()).map_err(|_| Error::Limit {
        context: "call-site count",
        value: call_sites.len() as u64,
        limit: u64::from(u32::MAX),
    })?);
    for call_site in call_sites {
        output.write_u64(call_site.return_offset);
        output.write_u8(call_site.flags);
        output.write_u32(
            u32::try_from(call_site.match_regex.len()).map_err(|_| Error::Limit {
                context: "call-site regex count",
                value: call_site.match_regex.len() as u64,
                limit: u64::from(u32::MAX),
            })?,
        );
        for offset in &call_site.match_regex {
            output.write_uint(*offset, string_offset_size)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::endian::Endian;
    use crate::model::{AddressRange, LineEntry};

    use super::{EncodedCallSite, EncodedFunction, EncodedInlineNode, decode_exact, encode};

    fn rich_function() -> EncodedFunction {
        EncodedFunction {
            range: AddressRange::new(0x1000, 0x1040),
            name: 1,
            lines: Some(vec![
                LineEntry {
                    address: 0x1000,
                    file: 1.into(),
                    line: 10,
                },
                LineEntry {
                    address: 0x1010,
                    file: 2.into(),
                    line: 12,
                },
            ]),
            inline: Some(EncodedInlineNode {
                ranges: vec![AddressRange::new(0x1000, 0x1040)],
                name: 1,
                call_file: 0,
                call_line: 0,
                children: vec![EncodedInlineNode {
                    ranges: vec![AddressRange::new(0x1010, 0x1020)],
                    name: 5,
                    call_file: 2,
                    call_line: 22,
                    children: Vec::new(),
                }],
            }),
            merged: vec![EncodedFunction {
                range: AddressRange::new(0x1000, 0x1040),
                name: 9,
                ..EncodedFunction::default()
            }],
            call_sites: vec![EncodedCallSite {
                return_offset: 0x18,
                flags: 1,
                match_regex: vec![13, 21],
            }],
        }
    }

    #[test]
    fn all_records_round_trip_for_both_versions_and_endians() {
        let expected = rich_function();
        for endian in [Endian::Little, Endian::Big] {
            for string_offset_size in [4, 8] {
                let bytes = encode(&expected, endian, string_offset_size).unwrap();
                let actual = decode_exact(&bytes, endian, string_offset_size, 0x1000).unwrap();
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn function_info_minimal_bytes_match_wire_layout() {
        let function = EncodedFunction {
            range: AddressRange::new(0x1000, 0x1100),
            name: 1,
            ..EncodedFunction::default()
        };
        let bytes = encode(&function, Endian::Little, 4).unwrap();
        assert_eq!(
            bytes,
            [
                0x00, 0x01, 0x00, 0x00, // Size
                0x01, 0x00, 0x00, 0x00, // Name
                0x00, 0x00, 0x00, 0x00, // EndOfList
                0x00, 0x00, 0x00, 0x00, // zero length
            ]
        );
    }
}

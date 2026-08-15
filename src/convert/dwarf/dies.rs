use std::collections::{HashMap, HashSet};

use gimli::{DebuggingInformationEntry, Dwarf, EntriesTreeNode, Reader, Unit};

use super::references::{absolute_entry_offset, resolve_name, resolve_reference_name};
use super::{file_index_attribute, gimli_error, unsigned_attribute};
use crate::convert::ConversionWarning;
use crate::model::{AddressRange, CallSite, CallSiteFlags, FileIndex, InlineNode};
use crate::{Error, Result};

struct Descendants {
    inlines: Vec<InlineNode>,
    call_sites: Vec<CallSite>,
    inline_count: usize,
}

pub(super) struct DetailOptions<'a> {
    pub(super) include_inlines: bool,
    pub(super) include_call_sites: bool,
    pub(super) warnings: &'a mut Vec<ConversionWarning>,
}

struct DetailContext<'data, 'warnings, R: Reader<Offset = usize>> {
    dwarf: &'data Dwarf<R>,
    unit: &'data Unit<R>,
    function_range: AddressRange,
    file_indices: &'data HashMap<u64, FileIndex>,
    include_inlines: bool,
    include_call_sites: bool,
    warnings: &'warnings mut Vec<ConversionWarning>,
}

pub(super) fn extract_subprogram_details<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    offset: gimli::UnitOffset<usize>,
    function_range: AddressRange,
    function_name: &[u8],
    file_indices: &HashMap<u64, FileIndex>,
    options: &mut DetailOptions<'_>,
) -> Result<(Option<InlineNode>, Vec<CallSite>, usize)> {
    if !options.include_inlines && !options.include_call_sites {
        return Ok((None, Vec::new(), 0));
    }
    let mut tree = unit.entries_tree(Some(offset)).map_err(gimli_error)?;
    let root = tree.root().map_err(gimli_error)?;
    let mut context = DetailContext {
        dwarf,
        unit,
        function_range,
        file_indices,
        include_inlines: options.include_inlines,
        include_call_sites: options.include_call_sites,
        warnings: options.warnings,
    };
    let descendants = collect_descendants(&mut context, root, &[function_range], 0)?;
    let inline = (!descendants.inlines.is_empty()).then(|| InlineNode {
        ranges: vec![function_range],
        name: function_name.to_vec(),
        call_file: FileIndex::ZERO,
        call_line: 0,
        children: descendants.inlines,
    });
    Ok((inline, descendants.call_sites, descendants.inline_count))
}

fn collect_descendants<R: Reader<Offset = usize>>(
    context: &mut DetailContext<'_, '_, R>,
    node: EntriesTreeNode<'_, '_, R>,
    parent_ranges: &[AddressRange],
    depth: usize,
) -> Result<Descendants> {
    if depth > 256 {
        return Err(Error::InvalidModel("DWARF DIE nesting exceeds 256 levels"));
    }
    let mut result = Descendants {
        inlines: Vec::new(),
        call_sites: Vec::new(),
        inline_count: 0,
    };
    let mut children = node.children();
    while let Some(child) = children.next().map_err(gimli_error)? {
        let entry = child.entry();
        let tag = entry.tag();
        let inline_data =
            if context.include_inlines && tag == gimli::constants::DW_TAG_inlined_subroutine {
                make_inline_node(
                    context.dwarf,
                    context.unit,
                    entry,
                    parent_ranges,
                    context.file_indices,
                    context.warnings,
                )?
            } else {
                None
            };
        let call_site = if context.include_call_sites
            && depth == 0
            && tag == gimli::constants::DW_TAG_call_site
        {
            make_call_site(context.dwarf, context.unit, entry, context.function_range)?
        } else {
            None
        };
        let nested_parent = inline_data
            .as_ref()
            .map_or(parent_ranges, |inline| inline.ranges.as_slice());
        let nested = collect_descendants(context, child, nested_parent, depth.saturating_add(1))?;
        if depth == 0 {
            result.call_sites.extend(nested.call_sites);
        }
        result.inline_count = result.inline_count.saturating_add(nested.inline_count);
        if let Some(call_site) = call_site {
            result.call_sites.push(call_site);
        }
        if let Some(mut inline) = inline_data {
            inline.children = nested.inlines;
            result.inline_count = result.inline_count.saturating_add(1);
            result.inlines.push(inline);
        } else {
            result.inlines.extend(nested.inlines);
        }
    }
    Ok(result)
}

fn make_inline_node<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    parent_ranges: &[AddressRange],
    file_indices: &HashMap<u64, FileIndex>,
    warnings: &mut Vec<ConversionWarning>,
) -> Result<Option<InlineNode>> {
    let Some(name) = resolve_name(dwarf, unit, entry, 0)? else {
        return Ok(None);
    };
    let mut ranges = dwarf.die_ranges(unit, entry).map_err(gimli_error)?;
    let mut valid_ranges = Vec::new();
    while let Some(range) = ranges.next().map_err(gimli_error)? {
        let candidate = AddressRange::new(range.begin, range.end);
        if range.begin < range.end
            && parent_ranges
                .iter()
                .any(|parent| parent.contains_range(candidate))
        {
            valid_ranges.push(candidate);
        }
    }
    valid_ranges.sort_unstable();
    if valid_ranges.is_empty() {
        return Ok(None);
    }
    let call_file = match file_index_attribute(entry, gimli::constants::DW_AT_call_file) {
        Some(dwarf_index) => {
            if let Some(index) = file_indices.get(&dwarf_index).copied() {
                index
            } else {
                warnings.push(ConversionWarning::MissingInlineCallFile {
                    die_offset: absolute_entry_offset(unit, entry.offset())? as u64,
                    index: dwarf_index,
                });
                FileIndex::ZERO
            }
        }
        None => FileIndex::ZERO,
    };
    let call_line = unsigned_attribute(entry, gimli::constants::DW_AT_call_line).unwrap_or(0);
    let call_line = if let Ok(line) = u32::try_from(call_line) {
        line
    } else {
        warnings.push(ConversionWarning::InvalidInlineCallLine {
            die_offset: absolute_entry_offset(unit, entry.offset())? as u64,
            line: call_line,
        });
        0
    };
    Ok(Some(InlineNode {
        ranges: valid_ranges,
        name,
        call_file,
        call_line,
        children: Vec::new(),
    }))
}

fn make_call_site<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    entry: &DebuggingInformationEntry<R>,
    function_range: AddressRange,
) -> Result<Option<CallSite>> {
    let Some(return_pc_value) = entry.attr_value(gimli::constants::DW_AT_call_return_pc) else {
        return Ok(None);
    };
    let Some(return_pc) = dwarf
        .attr_address(unit, return_pc_value)
        .map_err(gimli_error)?
    else {
        return Ok(None);
    };
    if !function_range.contains(return_pc) {
        return Ok(None);
    }
    let mut patterns = Vec::new();
    if let Some(origin) = entry.attr_value(gimli::constants::DW_AT_call_origin) {
        let mut visited = HashSet::new();
        if let Some(name) = resolve_reference_name(dwarf, unit, &origin, 0, &mut visited)? {
            patterns.push(name);
        }
    }
    Ok(Some(CallSite {
        return_offset: return_pc
            .checked_sub(function_range.start)
            .ok_or(Error::InvalidModel(
                "call-site return address precedes its function",
            ))?,
        flags: CallSiteFlags::default(),
        match_regex: patterns,
    }))
}

use std::collections::{HashMap, HashSet};

use gimli::{DebuggingInformationEntry, Dwarf, Reader, Unit};

use super::references::{absolute_entry_offset, resolve_name, resolve_reference_name};
use super::{file_index_attribute, gimli_error, unsigned_attribute};
use crate::Result;
use crate::convert::ConversionWarning;
use crate::format::function::check_inline_depth;
use crate::model::{AddressRange, CallSite, CallSiteFlags, FileIndex, InlineNode};

struct Descendants {
    inlines: Vec<InlineNode>,
    call_sites: Vec<CallSite>,
    inline_count: usize,
}

struct InlineFrame {
    die_depth: isize,
    node: InlineNode,
}

struct CallSiteFrame {
    die_depth: isize,
    call_site: CallSite,
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
    let mut context = DetailContext {
        dwarf,
        unit,
        function_range,
        file_indices,
        include_inlines: options.include_inlines,
        include_call_sites: options.include_call_sites,
        warnings: options.warnings,
    };
    let mut descendants = collect_descendants(&mut context, offset)?;
    descendants.inlines.shrink_to_fit();
    descendants.call_sites.shrink_to_fit();
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
    offset: gimli::UnitOffset<usize>,
) -> Result<Descendants> {
    let mut result = Descendants {
        inlines: Vec::new(),
        call_sites: Vec::new(),
        inline_count: 0,
    };
    let mut entries = context
        .unit
        .entries_at_offset(offset)
        .map_err(gimli_error)?;
    let Some(root) = entries.next_dfs().map_err(gimli_error)? else {
        return Ok(result);
    };
    let root_depth = root.depth();
    let mut inline_stack: Vec<InlineFrame> = Vec::new();
    let mut call_site_stack: Vec<CallSiteFrame> = Vec::new();
    while let Some(entry) = entries.next_dfs().map_err(gimli_error)? {
        if entry.depth() <= root_depth {
            break;
        }
        finish_call_sites(&mut call_site_stack, entry.depth(), &mut result.call_sites);
        while inline_stack
            .last()
            .is_some_and(|frame| entry.depth() <= frame.die_depth)
        {
            finish_inline(&mut inline_stack, &mut result.inlines);
        }
        let tag = entry.tag();
        if context.include_call_sites
            && tag == gimli::constants::DW_TAG_call_site
            && let Some(call_site) =
                make_call_site(context.dwarf, context.unit, entry, context.function_range)?
        {
            call_site_stack.push(CallSiteFrame {
                die_depth: entry.depth(),
                call_site,
            });
        }

        if context.include_inlines && tag == gimli::constants::DW_TAG_inlined_subroutine {
            let parent_ranges = inline_stack.last().map_or_else(
                || std::slice::from_ref(&context.function_range),
                |frame| frame.node.ranges.as_slice(),
            );
            let Some(node) = make_inline_node(
                context.dwarf,
                context.unit,
                entry,
                parent_ranges,
                context.file_indices,
                context.warnings,
            )?
            else {
                continue;
            };
            check_inline_depth(inline_stack.len().saturating_add(1))?;
            result.inline_count = result.inline_count.saturating_add(1);
            inline_stack.push(InlineFrame {
                die_depth: entry.depth(),
                node,
            });
        }
    }
    while !inline_stack.is_empty() {
        finish_inline(&mut inline_stack, &mut result.inlines);
    }
    finish_call_sites(&mut call_site_stack, root_depth, &mut result.call_sites);
    Ok(result)
}

fn finish_inline(stack: &mut Vec<InlineFrame>, roots: &mut Vec<InlineNode>) {
    let Some(mut frame) = stack.pop() else {
        return;
    };
    frame.node.ranges.shrink_to_fit();
    frame.node.name.shrink_to_fit();
    frame.node.children.shrink_to_fit();
    if let Some(parent) = stack.last_mut() {
        parent.node.children.push(frame.node);
    } else {
        roots.push(frame.node);
    }
}

fn finish_call_sites(
    stack: &mut Vec<CallSiteFrame>,
    next_depth: isize,
    output: &mut Vec<CallSite>,
) {
    while stack
        .last()
        .is_some_and(|frame| frame.die_depth >= next_depth)
    {
        if let Some(frame) = stack.pop() {
            output.push(frame.call_site);
        }
    }
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
    coalesce_ranges(&mut valid_ranges);
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

fn coalesce_ranges(ranges: &mut Vec<AddressRange>) {
    ranges.sort_unstable();
    ranges.dedup_by(|range, previous| {
        if range.start <= previous.end {
            previous.end = previous.end.max(range.end);
            true
        } else {
            false
        }
    });
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
    let Some(return_offset) = return_pc.checked_sub(function_range.start) else {
        return Ok(None);
    };
    let mut patterns = Vec::new();
    if let Some(origin) = entry.attr_value(gimli::constants::DW_AT_call_origin) {
        let mut visited = HashSet::new();
        if let Some(name) = resolve_reference_name(dwarf, unit, &origin, 0, &mut visited)? {
            patterns.push(name);
        }
    }
    patterns.shrink_to_fit();
    Ok(Some(CallSite {
        return_offset,
        flags: CallSiteFlags::default(),
        match_regex: patterns,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gimli::{DwarfSections, EndianSlice, LittleEndian, SectionId};

    #[test]
    fn inline_range_unions_are_sorted_and_coalesced() {
        let mut ranges = vec![
            AddressRange::new(0x20, 0x30),
            AddressRange::new(0x10, 0x18),
            AddressRange::new(0x14, 0x24),
            AddressRange::new(0x10, 0x18),
            AddressRange::new(0x40, 0x48),
            AddressRange::new(0x48, 0x50),
        ];
        coalesce_ranges(&mut ranges);
        assert_eq!(
            ranges,
            [AddressRange::new(0x10, 0x30), AddressRange::new(0x40, 0x50),]
        );
    }

    #[test]
    fn call_sites_are_finished_in_depth_first_postorder() {
        let call_site = |return_offset| CallSite {
            return_offset,
            ..CallSite::default()
        };
        let mut stack = vec![CallSiteFrame {
            die_depth: 1,
            call_site: call_site(1),
        }];
        stack.push(CallSiteFrame {
            die_depth: 2,
            call_site: call_site(2),
        });
        let mut output = Vec::new();
        finish_call_sites(&mut stack, 2, &mut output);
        stack.push(CallSiteFrame {
            die_depth: 2,
            call_site: call_site(3),
        });
        finish_call_sites(&mut stack, 0, &mut output);

        assert_eq!(
            output
                .iter()
                .map(|call_site| call_site.return_offset)
                .collect::<Vec<_>>(),
            [2, 3, 1]
        );
    }

    #[test]
    fn non_inline_die_nesting_does_not_consume_the_inline_depth_limit() {
        let abbreviations = vec![
            1, 0x11, 1, 0, 0, // Compile unit with children.
            2, 0x2e, 1, 0, 0, // Subprogram with children.
            3, 0x0b, 1, 0, 0, // Lexical block with children.
            4, 0x1d, 0, // Inlined subroutine without children.
            0x03, 0x08, // DW_AT_name, DW_FORM_string.
            0x55, 0x17, // DW_AT_ranges, DW_FORM_sec_offset.
            0, 0, 0,
        ];
        let mut entries = vec![1, 2];
        entries.extend(std::iter::repeat_n(3, 300));
        entries.push(4);
        entries.extend_from_slice(b"deep_inline\0");
        entries.extend_from_slice(&0_u32.to_le_bytes());
        entries.extend(std::iter::repeat_n(0, 302));

        let mut debug_ranges = Vec::new();
        for (begin, end) in [
            (0x1018_u64, 0x1030_u64),
            (0x1010, 0x1020),
            (0x1010, 0x1020),
            (0x1040, 0x1050),
            (0, 0),
        ] {
            debug_ranges.extend_from_slice(&begin.to_le_bytes());
            debug_ranges.extend_from_slice(&end.to_le_bytes());
        }

        let unit_length = u32::try_from(7 + entries.len()).unwrap();
        let mut debug_info = Vec::new();
        debug_info.extend_from_slice(&unit_length.to_le_bytes());
        debug_info.extend_from_slice(&4_u16.to_le_bytes());
        debug_info.extend_from_slice(&0_u32.to_le_bytes());
        debug_info.push(8);
        debug_info.extend(entries);
        let sections = DwarfSections::load(|id| -> gimli::Result<Vec<u8>> {
            Ok(if id == SectionId::DebugAbbrev {
                abbreviations.clone()
            } else if id == SectionId::DebugInfo {
                debug_info.clone()
            } else if id == SectionId::DebugRanges {
                debug_ranges.clone()
            } else {
                Vec::new()
            })
        })
        .unwrap();
        let dwarf = sections.borrow(|section| EndianSlice::new(section, LittleEndian));
        let header = dwarf.units().next().unwrap().unwrap();
        let unit = dwarf.unit(header).unwrap();
        let mut entries = unit.entries();
        entries.next_dfs().unwrap().unwrap();
        let subprogram_offset = entries.next_dfs().unwrap().unwrap().offset();
        let mut warnings = Vec::new();
        let (inline, call_sites, count) = extract_subprogram_details(
            &dwarf,
            &unit,
            subprogram_offset,
            AddressRange::new(0x1000, 0x1100),
            b"root",
            &HashMap::new(),
            &mut DetailOptions {
                include_inlines: true,
                include_call_sites: false,
                warnings: &mut warnings,
            },
        )
        .unwrap();

        assert!(call_sites.is_empty());
        assert_eq!(count, 1);
        let root = inline.unwrap();
        let [child] = root.children.as_slice() else {
            panic!("expected exactly one inline child");
        };
        assert_eq!(child.name, b"deep_inline");
        assert_eq!(
            child.ranges,
            [
                AddressRange::new(0x1010, 0x1030),
                AddressRange::new(0x1040, 0x1050),
            ]
        );
    }
}

use crate::error::Result;
use crate::format::function::{EncodedFunction, EncodedInlineNode, check_merged_depth};
use crate::model::{CallSite, Function, InlineNode};

use super::function::FunctionRef;

pub(super) fn validate(
    reference: &FunctionRef<'_>,
    function: &EncodedFunction,
    verified_files: &mut [bool],
) -> Result<()> {
    validate_at(reference, function, None, 0, verified_files)
}

fn validate_at(
    reference: &FunctionRef<'_>,
    function: &EncodedFunction,
    merged_parent: Option<crate::AddressRange>,
    depth: usize,
    verified_files: &mut [bool],
) -> Result<()> {
    check_merged_depth(depth)?;
    validate_semantics(function, merged_parent)?;
    let _ = reference.string(function.name)?;
    for line in function.lines.iter().flatten() {
        verify_file(reference, line.file, verified_files)?;
    }
    if let Some(node) = &function.inline {
        validate_inline(reference, node, verified_files)?;
    }
    for site in &function.call_sites {
        for offset in &site.match_regex {
            let _ = reference.string(*offset)?;
        }
    }
    for merged in &function.merged {
        validate_at(
            reference,
            merged,
            Some(function.range),
            depth.saturating_add(1),
            verified_files,
        )?;
    }
    Ok(())
}

fn validate_semantics(
    function: &EncodedFunction,
    merged_parent: Option<crate::AddressRange>,
) -> Result<()> {
    if !function.range.is_valid() {
        return Err(crate::Error::InvalidFormat("reversed function range"));
    }
    if merged_parent.is_some_and(|parent| function.range != parent) {
        return Err(crate::Error::InvalidFormat(
            "merged function range differs from its parent",
        ));
    }
    if function.lines.iter().flatten().any(|line| {
        line.address < function.range.start
            || (!function.range.is_empty() && line.address >= function.range.end)
    }) {
        return Err(crate::Error::InvalidFormat(
            "line address is outside its function",
        ));
    }
    if let Some(inline) = &function.inline {
        validate_inline_ranges(inline, &[function.range])?;
    }
    let size = function.range.size();
    if function
        .call_sites
        .iter()
        .any(|site| size != 0 && site.return_offset >= size)
    {
        return Err(crate::Error::InvalidFormat(
            "call-site return offset is outside its function",
        ));
    }
    Ok(())
}

fn validate_inline_ranges(node: &EncodedInlineNode, parents: &[crate::AddressRange]) -> Result<()> {
    if node.ranges.is_empty()
        || node.ranges.iter().any(|range| {
            !range.is_valid()
                || range.is_empty()
                || !parents.iter().any(|parent| parent.contains_range(*range))
        })
    {
        return Err(crate::Error::InvalidFormat(
            "inline range is empty, reversed, or outside its parent",
        ));
    }
    for child in &node.children {
        validate_inline_ranges(child, &node.ranges)?;
    }
    Ok(())
}

fn validate_inline(
    reference: &FunctionRef<'_>,
    node: &EncodedInlineNode,
    verified_files: &mut [bool],
) -> Result<()> {
    if node.name != 0 {
        let _ = reference.string(node.name)?;
    }
    verify_file(reference, node.call_file.into(), verified_files)?;
    for child in &node.children {
        validate_inline(reference, child, verified_files)?;
    }
    Ok(())
}

fn verify_file(
    reference: &FunctionRef<'_>,
    index: crate::FileIndex,
    verified: &mut [bool],
) -> Result<()> {
    let slot = verified.get_mut(index.get() as usize);
    if slot.as_ref().is_some_and(|valid| **valid) {
        return Ok(());
    }
    let _ = reference.file(index)?;
    if let Some(slot) = slot {
        *slot = true;
    }
    Ok(())
}

pub(super) fn decode(reference: &FunctionRef<'_>, encoded: EncodedFunction) -> Result<Function> {
    decode_at::<true>(reference, encoded, None, 0)
}

// verify_with has already checked this record, including all nested records.
pub(super) fn decode_validated(
    reference: &FunctionRef<'_>,
    encoded: EncodedFunction,
) -> Result<Function> {
    decode_at::<false>(reference, encoded, None, 0)
}

fn decode_at<const VALIDATE: bool>(
    reference: &FunctionRef<'_>,
    encoded: EncodedFunction,
    merged_parent: Option<crate::AddressRange>,
    depth: usize,
) -> Result<Function> {
    if VALIDATE {
        check_merged_depth(depth)?;
        validate_semantics(&encoded, merged_parent)?;
        for line in encoded.lines.iter().flatten() {
            let _ = reference.file(line.file)?;
        }
    }
    let range = encoded.range;
    let resolve = |offset| reference.string(offset).map(<[u8]>::to_vec);
    let merged = encoded
        .merged
        .into_iter()
        .map(|item| decode_at::<VALIDATE>(reference, item, Some(range), depth.saturating_add(1)))
        .collect::<Result<_>>()?;
    let call_sites = encoded
        .call_sites
        .into_iter()
        .map(|site| {
            Ok(CallSite {
                return_offset: site.return_offset,
                flags: site.flags.into(),
                match_regex: site
                    .match_regex
                    .into_iter()
                    .map(&resolve)
                    .collect::<Result<_>>()?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Function {
        range: encoded.range,
        name: resolve(encoded.name)?,
        lines: encoded.lines.unwrap_or_default(),
        inline: encoded
            .inline
            .map(|node| decode_inline::<VALIDATE>(reference, node))
            .transpose()?,
        merged,
        call_sites,
    })
}

fn decode_inline<const VALIDATE: bool>(
    reference: &FunctionRef<'_>,
    node: EncodedInlineNode,
) -> Result<InlineNode> {
    if VALIDATE {
        let _ = reference.file(node.call_file.into())?;
    }
    Ok(InlineNode {
        ranges: node.ranges,
        name: if node.name == 0 {
            Vec::new()
        } else {
            reference.string(node.name)?.to_vec()
        },
        call_file: node.call_file.into(),
        call_line: node.call_line,
        children: node
            .children
            .into_iter()
            .map(|child| decode_inline::<VALIDATE>(reference, child))
            .collect::<Result<_>>()?,
    })
}

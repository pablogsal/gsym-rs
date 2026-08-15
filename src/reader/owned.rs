use crate::error::Result;
use crate::format::function::{EncodedFunction, EncodedInlineNode};
use crate::model::{CallSite, Function, InlineNode};

use super::function::FunctionRef;

pub(super) fn validate<D: AsRef<[u8]>>(
    gsym: &super::Gsym<D>,
    function: &EncodedFunction,
) -> Result<()> {
    validate_semantics(function, None)?;
    let _ = gsym.string(function.name)?;
    for line in function.lines.iter().flatten() {
        let _ = gsym.file(line.file)?;
    }
    if let Some(node) = &function.inline {
        validate_inline(gsym, node)?;
    }
    for site in &function.call_sites {
        for offset in &site.match_regex {
            let _ = gsym.string(*offset)?;
        }
    }
    for merged in &function.merged {
        validate(gsym, merged)?;
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
    for merged in &function.merged {
        validate_semantics(merged, Some(function.range))?;
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

fn validate_inline<D: AsRef<[u8]>>(gsym: &super::Gsym<D>, node: &EncodedInlineNode) -> Result<()> {
    if node.name != 0 {
        let _ = gsym.string(node.name)?;
    }
    let _ = gsym.file(node.call_file)?;
    for child in &node.children {
        validate_inline(gsym, child)?;
    }
    Ok(())
}

pub(super) fn decode(reference: &FunctionRef<'_>, encoded: EncodedFunction) -> Result<Function> {
    let resolve = |offset| reference.string(offset).map(<[u8]>::to_vec);
    let merged = encoded
        .merged
        .into_iter()
        .map(|item| decode(reference, item))
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
    for line in encoded.lines.iter().flatten() {
        let _ = reference.file(line.file)?;
    }
    Ok(Function {
        range: encoded.range,
        name: resolve(encoded.name)?,
        lines: encoded.lines.unwrap_or_default(),
        inline: encoded
            .inline
            .map(|node| decode_inline(reference, node))
            .transpose()?,
        merged,
        call_sites,
    })
}

fn decode_inline(reference: &FunctionRef<'_>, node: EncodedInlineNode) -> Result<InlineNode> {
    let _ = reference.file(node.call_file.into())?;
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
            .map(|child| decode_inline(reference, child))
            .collect::<Result<_>>()?,
    })
}

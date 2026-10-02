use crate::format::function::{check_inline_depth, check_merged_depth};
use crate::model::{AddressRange, FileEntry, FileIndex, Function, InlineNode};
use crate::{Error, Result};

#[derive(Clone, Copy)]
enum FileReferenceKind {
    Line,
    InlineCall,
}

pub(crate) fn validate_file_table(files: &[FileEntry]) -> Result<()> {
    if files
        .first()
        .is_none_or(|file| !file.directory.is_empty() || !file.basename.is_empty())
    {
        return Err(Error::InvalidModel("file-table index zero must be empty"));
    }
    Ok(())
}

pub(crate) fn validate_for_builder(function: &Function) -> Result<FileIndex> {
    validate_function_tree(function, None, 0)
}

// GsymBuilder validates structure before taking ownership and never exposes
// mutable functions. Only file references can become valid after insertion.
pub(crate) fn validate_function_files(function: &Function, file_count: usize) -> Result<()> {
    for line in &function.lines {
        validate_file_index(line.file, file_count, FileReferenceKind::Line)?;
    }
    if let Some(inline) = &function.inline {
        validate_inline_files(inline, file_count)?;
    }
    for merged in &function.merged {
        validate_function_files(merged, file_count)?;
    }
    Ok(())
}

fn validate_inline_files(node: &InlineNode, file_count: usize) -> Result<()> {
    validate_file_index(node.call_file, file_count, FileReferenceKind::InlineCall)?;
    for child in &node.children {
        validate_inline_files(child, file_count)?;
    }
    Ok(())
}

fn validate_function_tree(
    function: &Function,
    merged_parent: Option<AddressRange>,
    merged_depth: usize,
) -> Result<FileIndex> {
    check_merged_depth(merged_depth)?;
    if function.name.is_empty() {
        return Err(Error::InvalidModel("function name must not be empty"));
    }
    if !function.range.is_valid() {
        return Err(Error::InvalidModel("function range end precedes its start"));
    }
    if function.range.size() > u64::from(u32::MAX) {
        return Err(Error::Limit {
            context: "function size",
            value: function.range.size(),
            limit: u64::from(u32::MAX),
        });
    }
    if merged_parent.is_some_and(|parent| function.range != parent) {
        return Err(Error::InvalidModel(
            "merged function range differs from its parent",
        ));
    }
    let mut maximum_file = FileIndex::ZERO;
    for line in &function.lines {
        if line.address < function.range.start
            || (!function.range.is_empty() && line.address >= function.range.end)
        {
            return Err(Error::InvalidModel("line address is outside its function"));
        }
        maximum_file = maximum_file.max(line.file);
    }
    if let Some(inline) = &function.inline {
        maximum_file = maximum_file.max(validate_inline(inline, &[function.range], 0)?);
    }
    let size = function.range.size();
    if function
        .call_sites
        .iter()
        .any(|call_site| size != 0 && call_site.return_offset >= size)
    {
        return Err(Error::InvalidModel(
            "call-site return offset is outside its function",
        ));
    }
    for merged in &function.merged {
        maximum_file = maximum_file.max(validate_function_tree(
            merged,
            Some(function.range),
            merged_depth.saturating_add(1),
        )?);
    }
    Ok(maximum_file)
}

fn validate_inline(node: &InlineNode, parents: &[AddressRange], depth: usize) -> Result<FileIndex> {
    check_inline_depth(depth)?;
    if node.ranges.is_empty() {
        return Err(Error::InvalidModel("inline node must contain a range"));
    }
    let mut previous_end = None;
    for range in &node.ranges {
        if !range.is_valid()
            || range.is_empty()
            || !parents.iter().any(|parent| parent.contains_range(*range))
        {
            return Err(Error::InvalidModel(
                "inline range is empty, reversed, or outside its parent",
            ));
        }
        if previous_end.is_some_and(|end| range.start < end) {
            return Err(Error::InvalidModel(
                "inline ranges overlap or are not sorted",
            ));
        }
        previous_end = Some(range.end);
    }
    let mut maximum_file = node.call_file;
    for child in &node.children {
        maximum_file = maximum_file.max(validate_inline(
            child,
            &node.ranges,
            depth.saturating_add(1),
        )?);
    }
    Ok(maximum_file)
}

const fn validate_file_index(
    index: FileIndex,
    file_count: usize,
    kind: FileReferenceKind,
) -> Result<()> {
    if index.get() as usize >= file_count {
        return Err(Error::InvalidModel(match kind {
            FileReferenceKind::Line => "line references a missing file",
            FileReferenceKind::InlineCall => "inline call site references a missing file",
        }));
    }
    Ok(())
}

use crate::builder::BuilderOptions;
use crate::model::{AddressRange, Function, InlineNode};
use crate::normalize::compact_function_lines;

pub(super) fn finalize(functions: Vec<Function>, options: &BuilderOptions) -> Vec<Function> {
    let mut functions = functions;
    compact_function_lines(&mut functions);
    let functions = sort_by_ordering_key(functions);
    let functions = if options.merge_equal_address_functions {
        merge_equal_ranges(functions)
    } else {
        deduplicate(functions)
    };
    repair_final_range(functions, options)
}

fn sort_by_ordering_key(functions: Vec<Function>) -> Vec<Function> {
    let mut decorated: Vec<(Richness, Tiebreak, Function)> = functions
        .into_iter()
        .map(|function| (richness(&function), semantic_tiebreak(&function), function))
        .collect();
    decorated.sort_by(|left, right| {
        (left.2.range, left.0, left.2.name.as_slice(), left.1).cmp(&(
            right.2.range,
            right.0,
            right.2.name.as_slice(),
            right.1,
        ))
    });
    decorated
        .into_iter()
        .map(|(_, _, function)| function)
        .collect()
}

fn merge_equal_ranges(functions: Vec<Function>) -> Vec<Function> {
    let mut merged: Vec<Function> = Vec::with_capacity(functions.len());
    for function in functions {
        if let Some(parent) = merged.last_mut()
            && parent.range == function.range
        {
            if parent
                .merged
                .last()
                .is_none_or(|candidate| *candidate != function)
            {
                parent.merged.push(function);
            }
        } else {
            merged.push(function);
        }
    }
    merged
}

fn deduplicate(functions: Vec<Function>) -> Vec<Function> {
    let mut deduplicated: Vec<Function> = Vec::with_capacity(functions.len());
    for mut function in functions {
        if let Some(previous) = deduplicated.last_mut()
            && previous.range == function.range
        {
            let previous_rich = has_rich_info(previous);
            let current_rich = has_rich_info(&function);
            if previous_rich != current_rich {
                if !previous_rich
                    && should_replace_with_mangled_name(&previous.name, &function.name)
                {
                    function.name.clone_from(&previous.name);
                }
                if current_rich {
                    *previous = function;
                }
            } else if *previous != function {
                *previous = function;
            }
            continue;
        }
        if let Some(previous) = deduplicated.last_mut()
            && previous.range.is_empty()
            && function.range.contains(previous.range.start)
        {
            *previous = function;
            continue;
        }
        deduplicated.push(function);
    }
    deduplicated
}

fn repair_final_range(mut functions: Vec<Function>, options: &BuilderOptions) -> Vec<Function> {
    if options.repair_zero_sized_functions
        && let Some(last) = functions.last_mut()
        && last.range.is_empty()
    {
        let start = last.range.start;
        let end = options
            .executable_ranges
            .iter()
            .find(|range| range.contains(start))
            .map_or(start, |range| range.end);
        let repaired = AddressRange::new(
            start,
            start.saturating_add(end.saturating_sub(start).min(u64::from(u32::MAX))),
        );
        repair_merged_ranges(last, repaired);
    }
    functions
}

fn repair_merged_ranges(function: &mut Function, repaired: AddressRange) {
    function.range = repaired;
    for merged in &mut function.merged {
        repair_merged_ranges(merged, repaired);
    }
}

type Richness = (bool, usize, usize, usize, usize, usize, usize);

type Tiebreak = (usize, usize);

fn richness(function: &Function) -> Richness {
    let inline = function.inline.as_ref().map_or((0, 0, 0), inline_quality);
    (
        function.inline.is_some(),
        inline.0,
        inline.1,
        inline.2,
        function.lines.len(),
        function.call_sites.len().saturating_add(
            function
                .call_sites
                .iter()
                .map(|site| site.match_regex.len())
                .sum::<usize>(),
        ),
        function.merged.len(),
    )
}

fn inline_quality(node: &InlineNode) -> (usize, usize, usize) {
    node.children.iter().fold(
        (1, node.ranges.len(), 1),
        |(nodes, ranges, depth), child| {
            let child = inline_quality(child);
            (
                nodes.saturating_add(child.0),
                ranges.saturating_add(child.1),
                depth.max(child.2.saturating_add(1)),
            )
        },
    )
}

const fn has_rich_info(function: &Function) -> bool {
    !function.lines.is_empty() || function.inline.is_some() || !function.call_sites.is_empty()
}

fn should_replace_with_mangled_name(alternate: &[u8], current: &[u8]) -> bool {
    if current.is_empty() {
        return !alternate.is_empty();
    }
    if is_supported_mangled(current) || !is_supported_mangled(alternate) {
        return false;
    }
    let mut token = current.len().to_string().into_bytes();
    token.extend_from_slice(current);
    alternate.windows(token.len()).any(|window| window == token)
}

fn is_supported_mangled(name: &[u8]) -> bool {
    name.starts_with(b"_Z") || name.starts_with(b"$s") || name.starts_with(b"$S")
}

fn semantic_tiebreak(function: &Function) -> Tiebreak {
    let inline_children = function
        .inline
        .as_ref()
        .map_or(0, |node| node.children.len());
    (inline_children, function.name.len())
}

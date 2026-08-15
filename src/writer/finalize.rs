use crate::builder::{BuilderOptions, FunctionSetPolicy};
use crate::model::{AddressRange, Function, InlineNode};
use crate::normalize::compact_function_lines;

pub(super) fn finalize(
    functions: Vec<Function>,
    options: &BuilderOptions,
    function_set: FunctionSetPolicy,
) -> Vec<Function> {
    let mut functions = functions;
    compact_function_lines(&mut functions);
    let functions = sort_by_ordering_key(functions);
    let functions = match function_set {
        FunctionSetPolicy::MergeEqualRanges => merge_equal_ranges(functions),
        FunctionSetPolicy::Deduplicate => deduplicate(functions),
        FunctionSetPolicy::Preserve => functions,
    };
    repair_final_range(functions, options)
}

fn sort_by_ordering_key(mut functions: Vec<Function>) -> Vec<Function> {
    let mut keyed: Vec<_> = functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            (
                function.range,
                richness(function),
                function.name.as_slice(),
                semantic_tiebreak(function),
                index,
            )
        })
        .collect();
    keyed.sort_unstable();
    let mut order: Vec<usize> = keyed.into_iter().map(|entry| entry.4).collect();
    for start in 0..order.len() {
        let mut current = start;
        while let Some(source) = order
            .get(current)
            .copied()
            .filter(|source| *source != start)
        {
            functions.swap(current, source);
            if let Some(slot) = order.get_mut(current) {
                *slot = current;
            }
            current = source;
        }
        if let Some(slot) = order.get_mut(current) {
            *slot = current;
        }
    }
    functions
}

fn merge_equal_ranges(functions: Vec<Function>) -> Vec<Function> {
    let mut merged: Vec<Function> = Vec::with_capacity(functions.len());
    for function in functions {
        if let Some(parent) = merged.last_mut()
            && parent.range == function.range
        {
            let previous = parent.merged.last().unwrap_or(&*parent);
            if *previous != function {
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
        if repair_keeps_records(last, repaired) {
            repair_merged_ranges(last, repaired);
        }
    }
    functions
}

fn repair_keeps_records(function: &Function, repaired: AddressRange) -> bool {
    function
        .lines
        .iter()
        .all(|line| repaired.contains(line.address))
        && function
            .call_sites
            .iter()
            .all(|call_site| call_site.return_offset < repaired.size())
        && function.inline.as_ref().is_none_or(|inline| {
            inline
                .ranges
                .iter()
                .all(|range| repaired.contains_range(*range))
        })
        && function
            .merged
            .iter()
            .all(|merged| repair_keeps_records(merged, repaired))
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

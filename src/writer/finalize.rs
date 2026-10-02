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
    let mut order: Vec<_> = functions
        .iter()
        .enumerate()
        .map(|(index, function)| (function.range, index))
        .collect();
    order.sort_unstable();
    {
        // Tree richness and names only affect functions with equal ranges.
        // Reuse this scratch allocation across alias groups.
        let mut aliases = Vec::new();
        for group in order.chunk_by_mut(|left, right| left.0 == right.0) {
            if group.len() < 2 {
                continue;
            }
            let compare_inlines = group
                .iter()
                .filter(|(_, index)| {
                    functions
                        .get(*index)
                        .is_some_and(|function| function.inline.is_some())
                })
                .take(2)
                .count()
                == 2;
            aliases.clear();
            aliases.extend(group.iter().filter_map(|(_, index)| {
                let function = functions.get(*index)?;
                Some((
                    richness(function, compare_inlines),
                    function.name.as_slice(),
                    semantic_tiebreak(function),
                    *index,
                ))
            }));
            aliases.sort_unstable();
            for (slot, key) in group.iter_mut().zip(&aliases) {
                slot.1 = key.3;
            }
        }
    }
    for start in 0..order.len() {
        let mut current = start;
        while let Some(source) = order
            .get(current)
            .map(|entry| entry.1)
            .filter(|source| *source != start)
        {
            functions.swap(current, source);
            if let Some(slot) = order.get_mut(current) {
                slot.1 = current;
            }
            current = source;
        }
        if let Some(slot) = order.get_mut(current) {
            slot.1 = current;
        }
    }
    functions
}

fn merge_equal_ranges(mut functions: Vec<Function>) -> Vec<Function> {
    functions.dedup_by(|function, parent| {
        if parent.range != function.range {
            return false;
        }
        let previous = parent.merged.last().unwrap_or(&*parent);
        if *previous != *function {
            parent.merged.push(std::mem::take(function));
        }
        true
    });
    functions
}

fn deduplicate(mut functions: Vec<Function>) -> Vec<Function> {
    functions.dedup_by(|function, previous| {
        if previous.range == function.range {
            let previous_rich = has_rich_info(previous);
            let current_rich = has_rich_info(function);
            if previous_rich != current_rich {
                if !previous_rich
                    && should_replace_with_mangled_name(&previous.name, &function.name)
                {
                    function.name.clone_from(&previous.name);
                }
                if current_rich {
                    std::mem::swap(previous, function);
                }
            } else if *previous != *function {
                std::mem::swap(previous, function);
            }
            true
        } else if previous.range.is_empty() && function.range.contains(previous.range.start) {
            std::mem::swap(previous, function);
            true
        } else {
            false
        }
    });
    functions
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

fn richness(function: &Function, compare_inlines: bool) -> Richness {
    // A single inline tree already wins on inline presence, before its size or
    // depth can affect ordering. Avoid traversing that tree just for ranking.
    let inline = if compare_inlines {
        function.inline.as_ref().map_or((0, 0, 0), inline_quality)
    } else {
        (0, 0, 0)
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LineEntry;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn deferred_ranking_matches_the_complete_stable_key(
            records in prop::collection::vec((0_u8..12, 0_u8..8, 0_u8..6, 0_u8..8), 0..100),
        ) {
            let functions: Vec<_> = records.into_iter().map(|(address, name, children, rows)| {
                let range = AddressRange::new(u64::from(address).saturating_mul(16), u64::from(address).saturating_mul(16).saturating_add(8));
                let inline = (children > 0).then(|| InlineNode {
                    ranges: vec![range],
                    children: (0..children).map(|_| InlineNode {
                        ranges: vec![range],
                        ..InlineNode::default()
                    }).collect(),
                    ..InlineNode::default()
                });
                Function {
                    inline,
                    lines: (0..rows).map(|line| LineEntry::new(range.start, 0.into(), u32::from(line))).collect(),
                    ..Function::new(range, vec![name])
                }
            }).collect();
            let mut expected = functions.clone();
            expected.sort_by_cached_key(|function| (
                function.range,
                richness(function, true),
                function.name.clone(),
                semantic_tiebreak(function),
            ));
            prop_assert_eq!(sort_by_ordering_key(functions), expected);
        }
    }
}

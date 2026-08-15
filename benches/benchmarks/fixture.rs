use gsym::{
    AddressRange, CallSite, CallSiteFlags, Endian, FileEntry, Function, GsymBuilder, GsymVersion,
    InlineNode, LineEntry,
};

pub(crate) const BASE_ADDRESS: u64 = 0x0040_0000;
pub(crate) const FUNCTION_STRIDE: u64 = 64;
pub(crate) const FUNCTION_SIZE: u64 = 48;
pub(crate) const LOOKUP_OFFSET: u64 = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Shape {
    Lean,
    Rich,
}

impl Shape {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Lean => "lean",
            Self::Rich => "rich",
        }
    }
}

pub(crate) fn builder(
    version: GsymVersion,
    endian: Endian,
    count: usize,
    shape: Shape,
) -> GsymBuilder {
    let mut builder = GsymBuilder::new()
        .version(version)
        .endian(endian)
        .base_address(BASE_ADDRESS)
        .build_id(b"criterion-fixture".as_slice());
    let source = (shape == Shape::Rich)
        .then(|| builder.add_file(FileEntry::new(b"/benchmark/src", b"fixture.rs")))
        .transpose()
        .expect("fixture source file must fit the GSYM file table");

    for index in 0..count {
        let start = function_start(index);
        let range = AddressRange::new(start, start + FUNCTION_SIZE);
        let mut function = Function::new(range, format!("benchmark_function_{index:08}"));
        if let Some(source) = source {
            function.lines = vec![
                LineEntry::new(start, source, 100),
                LineEntry::new(start + 16, source, 101),
                LineEntry::new(start + 32, source, 102),
            ];
            function.inline = Some(InlineNode {
                ranges: vec![AddressRange::new(start + 8, start + 44)],
                name: b"outer_inline".to_vec(),
                call_file: source,
                call_line: 90,
                children: vec![InlineNode {
                    ranges: vec![AddressRange::new(start + 16, start + 36)],
                    name: b"inner_inline".to_vec(),
                    call_file: source,
                    call_line: 95,
                    children: Vec::new(),
                }],
            });
            function.call_sites.push(CallSite {
                return_offset: LOOKUP_OFFSET,
                flags: CallSiteFlags::INTERNAL,
                match_regex: vec![b"callee_one".to_vec(), b"callee_two.*".to_vec()],
            });
        }
        builder
            .add_function(function)
            .expect("benchmark function must satisfy writer invariants");
    }
    builder
}

pub(crate) fn encoded(version: GsymVersion, endian: Endian, count: usize, shape: Shape) -> Vec<u8> {
    builder(version, endian, count, shape)
        .to_bytes()
        .expect("benchmark fixture must encode")
}

pub(crate) const fn function_start(index: usize) -> u64 {
    BASE_ADDRESS + index as u64 * FUNCTION_STRIDE
}

pub(crate) const fn hit(index: usize) -> u64 {
    function_start(index) + LOOKUP_OFFSET
}

pub(crate) const fn gap(index: usize) -> u64 {
    function_start(index) + FUNCTION_SIZE + 4
}

pub(crate) fn sorted_hits(count: usize) -> Vec<u64> {
    (0..count).map(hit).collect()
}

pub(crate) fn shuffled_hits(count: usize) -> Vec<u64> {
    let mut addresses = sorted_hits(count);
    let mut state = 0x4d59_5347_u64;
    for index in (1..addresses.len()).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let choices = u64::try_from(index + 1).expect("fixture size must fit u64");
        let target = usize::try_from(state % choices).expect("shuffle index must fit usize");
        addresses.swap(index, target);
    }
    addresses
}

pub(crate) fn mixed_queries(count: usize) -> Vec<u64> {
    (0..count)
        .map(|index| {
            if index.is_multiple_of(4) {
                gap(index)
            } else {
                hit(index)
            }
        })
        .collect()
}

use std::hash::{BuildHasher, Hasher};

use hashbrown::{DefaultHashBuilder, HashTable};

use crate::Result;
use crate::format::function::{
    EncodedCallSite, EncodedFunction, EncodedInlineNode, check_merged_depth,
};
use crate::model::{FileEntry, Function, InlineNode};

const AVERAGE_STRING_LEN: usize = 24;

/// Deduplicates strings while preserving first-insertion order.
#[derive(Debug)]
pub(super) struct StringTable {
    bytes: Vec<u8>,
    entries: HashTable<StringEntry>,
    hasher: DefaultHashBuilder,
}

#[derive(Clone, Copy, Debug)]
struct StringEntry {
    offset: usize,
    len: usize,
    hash: u64,
}

fn interned(bytes: &[u8], entry: StringEntry) -> Option<&[u8]> {
    bytes.get(entry.offset..entry.offset.checked_add(entry.len)?)
}

impl StringTable {
    pub(super) fn with_capacity(strings: usize) -> Self {
        let mut table = Self {
            bytes: Vec::with_capacity(strings.saturating_mul(AVERAGE_STRING_LEN).max(1)),
            entries: HashTable::with_capacity(strings.saturating_add(1)),
            hasher: DefaultHashBuilder::default(),
        };
        table.bytes.push(0);
        table.record(0, &[], table.hash_bytes(&[]));
        table
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn intern(&mut self, string: &[u8]) -> u64 {
        let hash = self.hash_bytes(string);
        if let Some(offset) = self.find_hashed(string, hash) {
            return offset;
        }
        let offset = self.bytes.len();
        self.bytes.extend_from_slice(string);
        self.bytes.push(0);
        self.record(offset, string, hash);
        offset as u64
    }

    fn hash_bytes(&self, bytes: &[u8]) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        hasher.write(bytes);
        hasher.finish()
    }

    fn find_hashed(&self, string: &[u8], hash: u64) -> Option<u64> {
        self.entries
            .find(hash, |entry| interned(&self.bytes, *entry) == Some(string))
            .map(|entry| entry.offset as u64)
    }

    fn record(&mut self, offset: usize, string: &[u8], hash: u64) {
        let entry = StringEntry {
            offset,
            len: string.len(),
            hash,
        };
        self.entries
            .insert_unique(entry.hash, entry, |entry| entry.hash);
    }
}

pub(super) fn intern_files(files: &[FileEntry], strings: &mut StringTable) -> Vec<(u64, u64)> {
    files
        .iter()
        .map(|file| {
            (
                strings.intern(&file.directory),
                strings.intern(&file.basename),
            )
        })
        .collect()
}

pub(super) fn encode_function(
    function: Function,
    strings: &mut StringTable,
) -> Result<EncodedFunction> {
    encode_function_at(function, strings, 0)
}

fn encode_function_at(
    function: Function,
    strings: &mut StringTable,
    depth: usize,
) -> Result<EncodedFunction> {
    check_merged_depth(depth)?;
    let name = strings.intern(&function.name);
    let lines = (!function.lines.is_empty()).then_some(function.lines);
    let inline = function
        .inline
        .map(|inline| encode_inline(inline, strings))
        .transpose()?;
    let merged = function
        .merged
        .into_iter()
        .map(|entry| encode_function_at(entry, strings, depth.saturating_add(1)))
        .collect::<Result<Vec<_>>>()?;
    let call_sites = function
        .call_sites
        .into_iter()
        .map(|call_site| {
            let match_regex = call_site
                .match_regex
                .into_iter()
                .map(|pattern| strings.intern(&pattern))
                .collect();
            EncodedCallSite {
                return_offset: call_site.return_offset,
                flags: call_site.flags.bits(),
                match_regex,
            }
        })
        .collect();
    Ok(EncodedFunction {
        range: function.range,
        name,
        lines,
        inline,
        merged,
        call_sites,
    })
}

fn encode_inline(node: InlineNode, strings: &mut StringTable) -> Result<EncodedInlineNode> {
    let name = strings.intern(&node.name);
    let children = node
        .children
        .into_iter()
        .map(|child| encode_inline(child, strings))
        .collect::<Result<Vec<_>>>()?;
    Ok(EncodedInlineNode {
        ranges: node.ranges,
        name,
        call_file: node.call_file.into(),
        call_line: node.call_line,
        children,
    })
}

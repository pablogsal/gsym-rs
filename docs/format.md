# The GSYM file format

What is in a GSYM file, for debugging one by hand or comparing output against
`llvm-gsymutil`. Using the API does not require any of it; start with
[symbolication](crate::docs::symbolication) for that.

## What GSYM stores

Three things in a normal Linux binary can map an address to a name:

| Source | Answers | Cost |
| --- | --- | --- |
| ELF `.symtab` / `.dynsym` | function name | small, but no lines and no inlining |
| DWARF | everything, in full generality | large, and a full parse to answer anything |
| GSYM | name, file, line, inline stack | small, and one address without parsing the rest |

GSYM keeps the address-to-source mapping from DWARF and drops the rest. There
are no types, no variable locations, and no unwind rules, so a debugger still
needs DWARF. A crash reporter, profiler, or tracing agent usually does not.

The layout follows from that goal. The address index is a flat sorted array
that can be searched directly. Each function then has one contiguous record
holding its line table, inline tree, and call sites, and strings are stored once
in a shared table and referenced by offset. Resolving an address therefore
touches one function's metadata rather than the whole file.

## Version-independent rules

The first four bytes are the magic `GSYM` (`0x4753_594d`). A file is written in
the byte order of the image it describes, and the magic discloses which one: a
reader compares those bytes against the native and byte-swapped magic, and the
match selects the byte order for every fixed-width integer in the file. There is
no separate byte-order field.

A little-endian file therefore starts with the bytes `MYSG`, and a big-endian
file with `GSYM`. LEB128 values are byte-order independent.

Three reserved values hold in both versions and are enforced in both
directions:

- String offset 0 is the empty string, so the string table must begin with NUL.
- File index 0 is the empty file entry, so real files are numbered from 1.
- A `FunctionInfo` name offset of 0 is invalid.

## Version 1

Version 1 is the widely deployed encoding and this crate's default. Its header
is 48 bytes:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | magic |
| 4 | 2 | version (1) |
| 6 | 1 | address offset size (1, 2, 4, or 8) |
| 7 | 1 | UUID size (at most 20) |
| 8 | 8 | base address |
| 16 | 4 | number of addresses |
| 20 | 4 | string table offset |
| 24 | 4 | string table size |
| 28 | 20 | UUID storage, zero-padded |

There is no directory, so the sections follow the header in a fixed order at
fixed alignments:

```text
0                                                              end of file
+--------+---------------+-----------------+-------+--------+-----------------+
| header | address table | address-info    | file  | string | FunctionInfo    |
| 48 B   | N x 1/2/4/8 B | offsets N x 4 B | table | table  | records         |
+--------+---------------+-----------------+-------+--------+-----------------+
         ^ aligned to    ^ aligned to 4    ^ 4     ^ at the header's
           entry width                             string table offset
```

1. Address table: `N` entries of the header's address offset size, sorted
   ascending. Each entry is an offset from the base address, so the entry width
   can shrink with the size of the image. This is the index an address is
   resolved against.
2. Address-info offsets: one `u32` per address, holding the absolute file offset
   of that function's `FunctionInfo` record. The two arrays are parallel, so
   index *i* in one refers to index *i* in the other.
3. File table: a `u32` count followed by 8-byte entries of two `u32` string
   offsets, directory then basename.
4. String table: at the explicit offset in the header. It starts with a NUL
   byte, so offset 0 is the empty string.
5. `FunctionInfo` section: from the end of the string table to the end of the
   file.

Those `u32` fields set version 1's limits. String offsets and `FunctionInfo`
offsets cannot exceed 4 GiB, and a build ID longer than 20 bytes cannot be
stored. Files that need more must use version 2.

## Version 2

Version 2 replaces the fixed layout with a directory. That is what lifts those
limits. Its header is 20 bytes:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | magic |
| 4 | 2 | version (2) |
| 6 | 1 | address offset size |
| 7 | 1 | string table encoding (0) |
| 8 | 8 | base address |
| 16 | 4 | number of addresses |

The global-data directory follows immediately. Its entries are 20 bytes each and
are terminated by an all-zero entry:

```text
+--------+-------------------------+-------------------------------+
| header | directory               | sections, in directory order  |
| 20 B   | (type u32, offset u64,  | UUID, addresses, address-info,|
|        |  size u64) x K, then 0  | file table, strings, records  |
+--------+-------------------------+-------------------------------+
```

| Type | Section | Required |
| --- | --- | --- |
| 1 | address offsets | yes |
| 2 | address-info offsets | yes |
| 3 | string table | yes |
| 4 | file table | yes |
| 5 | `FunctionInfo` | yes |
| 6 | UUID | no |

A duplicate entry, a missing required entry, a zero-sized entry, or a section
reaching past the end of the input is rejected at parse time.

Three widths change with the directory. String offsets and file-table entries
become 8 bytes, so a file entry is 16. Address-info offsets become `u64` values
relative to the start of the `FunctionInfo` section instead of absolute file
offsets, and that removes the 4 GiB ceiling.

Version 2 is read by LLVM 23 and newer. Older tools cannot read it, so this
crate writes version 1 unless [`GsymVersion::V2`](crate::GsymVersion) is
selected.

## `FunctionInfo` records

One record holds everything known about one function. It starts with a `u32`
size in bytes, which added to the address from the address table gives the
function's half-open range, and a string offset for the name. A name offset of
zero is invalid, since offset zero is the empty string.

A sequence of optional records follows, terminated by a type-0, length-0 entry:

```text
u32 type
u32 payload length
u8[payload length] payload
```

This framing is what makes the format extensible: a reader that does not
recognize a type can skip it using the length. Address lookup skips such
records, while [`Gsym::decode_all`](crate::Gsym::decode_all) rejects them rather
than dropping data it could not write back.

### Line table (type 1)

A small line program in the style of DWARF's, scoped to one function. The
payload starts with a SLEB128 minimum line delta, a SLEB128 maximum line delta,
and a ULEB128 first line number. The row cursor starts at the function's start
address, file index 1, and that first line.

| Opcode | Meaning |
| --- | --- |
| 0 | end of sequence |
| 1 | set file to the following ULEB128 index |
| 2 | advance the address by the following ULEB128 delta, emit a row |
| 3 | advance the line by the following SLEB128 delta |
| ≥ 4 | special: advance both, emit a row |

A special opcode packs a line delta and an address delta into one byte. With
`range = maximum − minimum + 1` and `adjusted = opcode − 4`, the line advances by
`minimum + (adjusted % range)` and the address by `adjusted / range`. The writer
picks the delta window that encodes the most rows as single bytes, and most of
the line table's compression comes from that.

### Inline info (type 2)

A tree of inlined calls, stored depth-first. Each node holds a sorted list of
non-overlapping address ranges, encoded as ULEB128 pairs relative to the
parent's first address, then a `u8` flag for whether children follow, a string
offset for the inlined function's name, and ULEB128 call file and call line. The
call file and line describe where the call appears in the *parent*. A sibling
list ends with a node whose range count is zero.

Storing the call site on the child is what lets a lookup rebuild a call stack.
The innermost frame takes the function's own line row, and each outer frame takes
the call site recorded by the frame nested inside it.

In a valid tree the ranges are sorted, non-empty, non-overlapping, and
contained by the parent's.

### Merged functions (type 3)

A `u32` count followed by length-prefixed `FunctionInfo` records that share the
parent's start address. These are the aliases that identical-code folding
collapses onto one address. Writing them is opt-in through
[`FunctionSetPolicy::MergeEqualRanges`](crate::FunctionSetPolicy), which
matches `llvm-gsymutil`'s explicit merged-functions mode.

### Call sites (type 4)

A `u32` count followed by entries of a `u64` return-address offset from the
function start, a `u8` flag byte, a `u32` regex count, and that many string
offsets. The strings are patterns describing the callees that may return to
that address, which a consumer can use to check a reconstructed stack.

## Looking at a real file

This repository's `gsymtool` prints these structures, which is the quickest way
to check this description against a file in hand:

```console
gsymtool dump ./app.gsym              # header, tables, and counts
gsymtool dump ./app.gsym --functions  # every indexed function record
gsymtool verify ./app.gsym            # check the whole file
```

`llvm-gsymutil --verify --verbose` prints the same file from the upstream
implementation.

## Further reading

On GSYM:

- [`llvm::gsym`](https://llvm.org/doxygen/namespacellvm_1_1gsym.html),
  generated documentation for the structures named on this page
- [LLVM's GSYM implementation](https://github.com/llvm/llvm-project/tree/main/llvm/lib/DebugInfo/GSYM),
  the reference encoder and decoder
- [`llvm-gsymutil`](https://github.com/llvm/llvm-project/tree/main/llvm/tools/llvm-gsymutil),
  the upstream command-line tool

On the formats GSYM derives from:

- [DWARF 5 standard](https://dwarfstd.org/doc/DWARF5.pdf), including the
  line-number program that GSYM's line table simplifies
- [ELF gABI](https://refspecs.linuxfoundation.org/elf/gabi4+/contents.html) for
  sections, symbols, and program headers
- [LEB128](https://en.wikipedia.org/wiki/LEB128), the variable-length integer
  encoding used throughout the records

On where debug information lives, which matters for the `docs::conversion` page
(`convert` feature):

- [GDB: separate debug files](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Separate-Debug-Files.html)
  for `.gnu_debuglink` and build-ID lookup
- [GDB: MiniDebugInfo](https://sourceware.org/gdb/current/onlinedocs/gdb.html/MiniDebugInfo.html)
  for the compressed `.gnu_debugdata` section
- [debuginfod](https://sourceware.org/elfutils/Debuginfod.html) for fetching
  debug files over the network
- [Debug fission](https://gcc.gnu.org/wiki/DebugFission) for split DWARF, `.dwo`
  files, and `.dwp` packages

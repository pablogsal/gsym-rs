# Cookbook

Worked examples for each part of the public API. Feature-gated APIs have their
own pages: `docs::conversion` for `convert`, and `MappedGsym` for `mmap`.

GSYM stores names and paths as bytes, so the examples use byte strings
throughout.

## The semantic model

[`Function`](crate::Function) and its parts are plain owned data with public
fields: build them however you like and hand them to a builder. Two newtypes
keep unrelated integers apart: [`FileIndex`](crate::FileIndex) is a file-table
index, and [`CallSiteFlags`](crate::CallSiteFlags) is a flag set that keeps bits
this crate does not recognize.

```rust
use gsym::{
    AddressRange, CallSite, CallSiteFlags, FileEntry, FileIndex, Function,
    InlineNode, LineEntry,
};

// Ranges are half-open: [start, end).
let range = AddressRange::new(0x1000, 0x1020);
assert!(range.is_valid());
assert!(!range.is_empty());
assert_eq!(range.size(), 0x20);
assert!(range.contains(0x1010));
assert!(range.contains_range(AddressRange::new(0x1004, 0x1008)));

// File-table index zero is reserved for the empty entry.
let file_index = FileIndex::new(1);
assert_eq!(file_index.get(), 1);
assert_eq!(FileIndex::from(1_u32), file_index);
assert_eq!(u32::from(file_index), 1);
assert_eq!(u64::from(file_index), 1);
assert_eq!(FileIndex::ZERO.get(), 0);

let file = FileEntry::new(b"src", b"main.rs");
assert_eq!(file.basename, b"main.rs");

let line = LineEntry::new(0x1000, file_index, 12);

// An inline node describes where in its *parent* the inlined call appears.
let inline = InlineNode {
    ranges: vec![AddressRange::new(0x1004, 0x100c)],
    name: b"inlined".to_vec(),
    call_file: file_index,
    call_line: 8,
    children: Vec::new(),
};

// Unknown flag bits round-trip.
let mut flags = CallSiteFlags::INTERNAL | CallSiteFlags::EXTERNAL;
assert!(flags.contains(CallSiteFlags::INTERNAL));
assert_eq!(flags.bits(), 3);
flags |= CallSiteFlags::from_bits_retain(0x80);
assert_eq!(u8::from(flags), 0x83);
assert!(CallSiteFlags::default().is_empty());

let call_site = CallSite {
    return_offset: 4,
    flags,
    match_regex: vec![b"callee.*".to_vec()],
};

let alias = Function::new(range, b"alias");
let function = Function {
    lines: vec![line],
    inline: Some(inline),
    merged: vec![alias],
    call_sites: vec![call_site],
    ..Function::new(range, b"main")
};
assert_eq!(function.name, b"main");
```

## Building and writing

[`GsymBuilder`](crate::GsymBuilder) turns semantic records into an image.
Encoding is deterministic: the same inputs and options always produce the same
bytes. Chained setters cover the common cases, and
[`BuilderOptions`](crate::BuilderOptions) sets everything at once. Writing
consumes the builder.

```rust
use gsym::{
    AddressRange, BuilderOptions, Endian, FileEntry, Function, FunctionSetPolicy,
    GsymBuilder, GsymVersion, LineEntry, WriterOptions,
};

fn make_builder() -> gsym::Result<GsymBuilder> {
    let options = BuilderOptions {
        writer: WriterOptions {
            version: GsymVersion::V2,
            endian: Endian::Big,
            base_address: Some(0x2000),
            build_id: vec![1, 2, 3, 4],
        },
        executable_ranges: vec![AddressRange::new(0x2000, 0x3000)]
            .into_boxed_slice(),
        repair_zero_sized_functions: true,
        merge_equal_address_functions: false,
    };

    // Later setters win over the options they were constructed with.
    let mut builder = GsymBuilder::with_options(options)
        .version(GsymVersion::V1)
        .endian(Endian::Little)
        .base_address(0x2000)
        .build_id([0xaa, 0xbb])
        .repair_zero_sized_functions(true)
        .function_set(FunctionSetPolicy::Deduplicate)
        .executable_ranges([AddressRange::new(0x2000, 0x3000)]);

    // Files are interned: adding the same entry twice returns the same index.
    let source = builder.add_file(FileEntry::new(b"src", b"lib.rs"))?;
    builder.add_function(Function {
        lines: vec![LineEntry::new(0x2010, source, 20)],
        ..Function::new(AddressRange::new(0x2010, 0x2020), b"built")
    })?;

    assert_eq!(builder.options().writer.version, GsymVersion::V1);
    assert_eq!(builder.files().len(), 2);
    assert_eq!(builder.functions().len(), 1);
    Ok(builder)
}

// `to_bytes` and `write_to` produce identical output.
let bytes = make_builder()?.to_bytes()?;
assert!(!bytes.is_empty());

let mut sink = Vec::new();
make_builder()?.write_to(&mut sink)?;
assert_eq!(sink, bytes);

let empty = GsymBuilder::new();
assert!(empty.functions().is_empty());
# Ok::<(), gsym::Error>(())
```

## Reading a file

[`Gsym::open`](crate::Gsym::open) reads a path into an owned snapshot and
validates it:

```no_run
use gsym::Gsym;

let reader = Gsym::open("app.gsym")?;
assert!(reader.verify()?.functions > 0);
# Ok::<(), gsym::Error>(())
```

[`Gsym::parse`](crate::Gsym::parse) accepts any `D: AsRef<[u8]>` without copying
it, including a borrowed slice, a `Vec<u8>`, a `Box<[u8]>`, or an `Arc<[u8]>`.
Parsing validates the table layout, and the reader API is the same either way.

```rust
use gsym::{AddressRange, Function, Gsym, GsymBuilder};

let mut builder = GsymBuilder::new().build_id([0xde, 0xad]);
builder.add_function(Function::new(
    AddressRange::new(0x3000, 0x3010),
    b"reader",
))?;
let bytes = builder.to_bytes()?;

// Borrowed storage: no copy, and the reader points at the caller's bytes.
let reader: Gsym<&[u8]> = Gsym::parse(bytes.as_slice())?;
assert_eq!(reader.as_ref().as_ptr(), bytes.as_ptr());

// Header metadata borrows from the input.
let header: gsym::Header<'_> = reader.header();
assert_eq!(header.version, gsym::GsymVersion::V1);
assert_eq!(header.endian, gsym::Endian::Little);
assert_eq!(header.address_count, 1);
assert!(matches!(header.address_offset_size, 1 | 2 | 4 | 8));
assert_eq!(header.base_address, 0x3000);
assert_eq!(header.build_id, [0xde, 0xad]);
assert_eq!(reader.build_id(), [0xde, 0xad]);

// Reserved entries: string offset zero and file index zero are always empty.
assert_eq!(reader.string(0)?, b"");
assert_eq!(reader.file(0_u32)?, (&b""[..], &b""[..]));

// Function records can be reached by index without decoding them.
let direct: gsym::FunctionRef<'_> = reader.function(0)?;
assert_eq!(direct.index(), 0);
assert_eq!(direct.start(), 0x3000);
assert_eq!(direct.range(), AddressRange::new(0x3000, 0x3010));
assert_eq!(direct.name(), b"reader");
assert_eq!(direct.decode()?.name, b"reader");
assert!(reader.get_function(1)?.is_none()); // out of bounds, not an error

// Iteration follows address-table order.
let mut functions: gsym::Functions<'_, &[u8]> = reader.functions();
assert_eq!(functions.len(), 1);
assert_eq!(
    functions.next().transpose()?.map(|function| function.name()),
    Some(&b"reader"[..]),
);
assert!(functions.next().is_none());

// Verification checks the whole file, not just what one lookup reads.
let verified: gsym::VerifyReport = reader.verify()?;
assert_eq!(verified.functions, 1);
assert_eq!(verified.files, 1);
assert_eq!(verified.strings, 2);
assert!(verified.function_info_bytes > 0);

let owned = Gsym::parse(bytes.clone())?;
assert_eq!(owned.into_inner(), bytes);
# Ok::<(), gsym::Error>(())
```

## Looking up addresses

[`Gsym::lookup`](crate::Gsym::lookup) is the ergonomic path: it returns the
frames and call-site patterns for an address.
[`Gsym::lookup_with_options`](crate::Gsym::lookup_with_options) reuses
caller-owned scratch storage and can skip optional records.
[`Gsym::for_each_frame`](crate::Gsym::for_each_frame) also avoids allocating the
result collection by handing each frame to a closure.

```rust
use gsym::{
    AddressRange, CallSite, Error, FrameLookupOptions, Function, Gsym,
    GsymBuilder, LineEntry, LookupOptions, LookupScratch,
};

let mut builder = GsymBuilder::new();
let file = builder.add_file(gsym::FileEntry::new(b"src", b"lookup.rs"))?;
builder.add_function(Function {
    lines: vec![LineEntry::new(0x4000, file, 42)],
    call_sites: vec![CallSite {
        return_offset: 4,
        match_regex: vec![b"target.*".to_vec()],
        ..CallSite::default()
    }],
    ..Function::new(AddressRange::new(0x4000, 0x4010), b"lookup")
})?;
let bytes = builder.to_bytes()?;
let reader = Gsym::parse(bytes)?;

let hit: gsym::Lookup<'_> = reader
    .lookup(0x4004)?
    .ok_or(Error::InvalidModel("expected lookup hit"))?;
let frame: gsym::LookupFrame<'_> = hit.frames()[0];
assert_eq!(hit.address, 0x4004);
assert_eq!(hit.function, AddressRange::new(0x4000, 0x4010));
assert_eq!(frame.name, b"lookup");
assert_eq!(frame.directory, b"src");
assert_eq!(frame.basename, b"lookup.rs");
assert_eq!(frame.line, 42);
assert_eq!(frame.offset, 4);
assert!(!frame.inlined);
assert_eq!(hit.call_site_patterns()[0], b"target.*");

// Names only: the line program, inline tree, and call sites are never read.
let mut scratch = LookupScratch::with_capacity(8);
let lean = reader
    .lookup_with_options(
        0x4004,
        LookupOptions {
            line_information: false,
            inline_frames: false,
            call_sites: false,
        },
        &mut scratch,
    )?
    .ok_or(Error::InvalidModel("expected lookup hit"))?;
assert_eq!(lean.frames()[0].line, 0);
assert!(lean.call_site_patterns().is_empty());

// No allocation: frames go to a closure and scratch is reused.
let mut visited = 0;
let found = reader.for_each_frame(
    0x4004,
    FrameLookupOptions {
        line_information: true,
        inline_frames: true,
    },
    &mut scratch,
    |_| visited += 1,
)?;
assert!(found);
assert_eq!(visited, 1);
# Ok::<(), gsym::Error>(())
```

## Decoding, transcoding, and segmenting

[`Gsym::decode_all`](crate::Gsym::decode_all) turns a whole file back into owned
semantic records, which you can edit and re-encode. Use
[`to_builder`](crate::DecodedGsym::to_builder) when the decoded model is worth
keeping, and [`into_builder`](crate::DecodedGsym::into_builder) or the consuming
[`transcode`](crate::DecodedGsym::transcode) when it is not.
[`Gsym::transcode`](crate::Gsym::transcode) is the one-shot form for changing
version or byte order.

```rust
use gsym::{
    AddressRange, DecodedGsym, Endian, Function, Gsym, GsymBuilder, GsymSegment,
    GsymVersion, TranscodeOptions,
};

fn source() -> gsym::Result<Vec<u8>> {
    let mut builder = GsymBuilder::new();
    builder.add_function(Function::new(
        AddressRange::new(0x5000, 0x5010),
        b"transform",
    ))?;
    builder.to_bytes()
}

// `None` means "keep whatever the input used".
let options = TranscodeOptions {
    version: Some(GsymVersion::V2),
    endian: Some(Endian::Big),
};
let source = source()?;

let rewritten = Gsym::parse(source.as_slice())?.transcode(options)?;
assert_eq!(Gsym::parse(rewritten)?.header().version, GsymVersion::V2);

let decoded: DecodedGsym = Gsym::parse(source.as_slice())?.decode_all()?;
assert_eq!(decoded.source_version, GsymVersion::V1);
assert_eq!(decoded.source_endian, Endian::Little);
assert_eq!(decoded.base_address, 0x5000);
assert!(decoded.build_id.is_empty());
assert_eq!(decoded.files.len(), 1);
assert_eq!(decoded.functions.len(), 1);
let cloned_builder = decoded.to_builder(options)?;
assert_eq!(cloned_builder.functions().len(), 1);

let moved_builder = Gsym::parse(source.as_slice())?
    .decode_all()?
    .into_builder(options)?;
assert_eq!(moved_builder.functions().len(), 1);

let transcoded = Gsym::parse(source.as_slice())?
    .decode_all()?
    .transcode(options)?;
assert_eq!(Gsym::parse(transcoded)?.header().endian, Endian::Big);

let decoded = Gsym::parse(source)?.decode_all()?;
let segments: Vec<GsymSegment> = decoded.segments(4096, options)?;
assert_eq!(segments.len(), 1);
assert_eq!(segments[0].first_address, 0x5000);
assert_eq!(segments[0].end_address, 0x5010);
assert_eq!(segments[0].function_count, 1);
assert!(!segments[0].bytes().is_empty());
# Ok::<(), gsym::Error>(())
```

## Versions, byte order, and errors

[`Error`](crate::Error) is `#[non_exhaustive]`: match the cases that need
specific recovery and keep a fallback arm for the rest.

```rust
use gsym::{Endian, Error, Gsym, GsymVersion, Result};

assert!(matches!(Endian::native(), Endian::Little | Endian::Big));
assert_eq!(GsymVersion::default(), GsymVersion::V1);

fn validate(bytes: &[u8]) -> Result<()> {
    match Gsym::parse(bytes) {
        Ok(gsym) => {
            gsym.verify()?;
            Ok(())
        }
        // "Not a GSYM file at all" is often worth treating as a normal answer.
        Err(Error::InvalidMagic(magic)) => {
            assert_eq!(magic, 0);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

validate(&[0; 48])?;
# Ok::<(), gsym::Error>(())
```

## Further reading

- [Symbolication](crate::docs::symbolication) for unslid addresses, reading
  frames, storage choices, performance, and threading
- `docs::conversion` for building GSYM from ELF and DWARF (`convert` feature)
- [Format](crate::docs::format) for what the bytes these examples produce
  contain

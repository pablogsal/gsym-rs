# Symbolication

How to get correct answers out of a GSYM file at runtime: which address to ask
about, how to read the frames that come back, and what it costs.

## Which address to look up

A GSYM file records the virtual addresses found in the ELF image it was built
from. A running process rarely uses those addresses directly. A PIE executable
or shared object is mapped at a load bias chosen at load time, so a return
address captured from a live stack is `unslid + bias`. Subtract the bias before
looking an address up. Skipping this step is the usual reason a lookup returns
`None`, or returns a plausible but wrong function name.

The bias is the difference between where a module was mapped and the `p_vaddr`
of its first `PT_LOAD` segment. On Linux, `dl_iterate_phdr` reports both halves:
`dlpi_addr` is the bias, and the program headers give the segment addresses.

```rust
/// Converts a runtime address into the unslid address a GSYM file stores.
fn unslide(runtime_address: u64, load_bias: u64) -> Option<u64> {
    runtime_address.checked_sub(load_bias)
}

assert_eq!(unslide(0x7f00_0040_1120, 0x7f00_0000_0000), Some(0x40_1120));
```

Two consequences follow. A non-PIE executable (`ET_EXEC`) has a bias of zero, so
its runtime addresses are already unslid, and testing only against a non-PIE
binary will hide a missing subtraction. And each module has its own bias and its
own GSYM file, so symbolicating a stack that crosses shared-object boundaries
means picking the right file per frame first, then unsliding against that
module's bias.

For a return address on a stack, subtract one more byte before the lookup. A
return address points at the instruction after the call, which may belong to the
following line, or at the end of a function to the following function. Looking
up `return_address - 1` gives the frame that describes the call itself.

## Matching a file to a module

Conversion copies the image's GNU build ID into the GSYM header, and
[`Gsym::build_id`](crate::Gsym::build_id) reads it back. Comparing it against the
build ID of the loaded module catches the common case of symbolicating against a
rebuilt binary, where the addresses still resolve but the names come from a
different build.

```rust
use gsym::{AddressRange, Function, Gsym, GsymBuilder};

let mut builder = GsymBuilder::new().build_id([0x01, 0x02, 0x03, 0x04]);
builder.add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"main"))?;
let bytes = builder.to_bytes()?;

let gsym = Gsym::parse(&bytes)?;
let module_build_id = [0x01, 0x02, 0x03, 0x04];
assert_eq!(gsym.build_id(), module_build_id);
# Ok::<(), gsym::Error>(())
```

An empty build ID means the input had none. Treat that as unknown rather than as
a mismatch.

## Reading the result

A hit returns frames ordered innermost first. With no inlining that is one
frame. With inlining, frame 0 is the deepest inlined body containing the
address, the last frame is the function the linker emitted, and the frames
between them are inlined calls. Printing them in order gives the call stack the
optimizer erased.

Each frame's file and line describe that frame's own position. The innermost
frame gets the line row covering the address. Every outer frame gets the call
site recorded by the frame nested inside it, which is where the inlined call
appears in the outer function's source.

`offset` is the distance from the start of the frame's own range: the function
for a real frame, the inline range for an inlined one. It is what you print as
`function+0x24` when there is no line information.

```rust
use gsym::{AddressRange, FileEntry, Function, Gsym, GsymBuilder, LineEntry};

let mut builder = GsymBuilder::new();
let file = builder.add_file(FileEntry::new(b"/src", b"main.rs"))?;
builder.add_function(Function {
    lines: vec![LineEntry::new(0x1000, file, 7)],
    ..Function::new(AddressRange::new(0x1000, 0x1010), b"main")
})?;
let bytes = builder.to_bytes()?;
let gsym = Gsym::parse(&bytes)?;

let hit = gsym.lookup(0x1004)?.expect("covered address");
for frame in hit.frames() {
    println!(
        "{}{} at {}/{}:{} +0x{:x}",
        String::from_utf8_lossy(frame.name),
        if frame.inlined { " (inlined)" } else { "" },
        String::from_utf8_lossy(frame.directory),
        String::from_utf8_lossy(frame.basename),
        frame.line,
        frame.offset,
    );
}
# Ok::<(), gsym::Error>(())
```

Names and paths are byte slices, not `str`. GSYM stores whatever bytes the
producer stored, and this crate does not reject non-UTF-8 data, so a symbol from
a different mangling scheme or a path from a different locale still round-trips.
Render with `String::from_utf8_lossy`, or validate with `str::from_utf8` to
reject invalid data.

`Ok(None)` means no function covers the address. That is a normal answer for
padding between functions, for addresses from another module, and for an address
that was never unslid.

## Choosing storage

All three readers are the same type, [`Gsym<D>`](crate::Gsym), over different
byte storage, so the query API does not change with the choice:

| Entry point | Storage | Use when |
| --- | --- | --- |
| [`Gsym::open`](crate::Gsym::open) | owned `Vec<u8>` | the default; safe, no file-stability requirement |
| [`Gsym::parse`](crate::Gsym::parse) | anything `AsRef<[u8]>` | the bytes are already in memory, shared in an `Arc<[u8]>`, or came from somewhere other than a file |
| `MappedGsym::map` | opaque mapped bytes | the file is large, lookups are sparse, and the file is known to be immutable while mapped (`mmap` feature) |

Mapping is `unsafe` because of that last condition: if another process
truncates or rewrites the file while it is mapped, results borrowed from it can
observe changed bytes or become invalid. `Gsym::open` costs one read of the
whole file and carries no such requirement.

Parsing is cheap in every case, so opening a file per batch of lookups is fine,
and keeping one reader alive for the process is better.

## Performance and threading

[`Gsym::lookup`](crate::Gsym::lookup) allocates its result. When that matters,
two paths avoid it:

- [`Gsym::for_each_frame`](crate::Gsym::for_each_frame) hands each frame to a
  closure and allocates nothing, reusing a caller-owned
  [`LookupScratch`](crate::LookupScratch). Create one per thread with
  [`LookupScratch::with_capacity`](crate::LookupScratch::with_capacity) and pass
  it to every lookup.
- [`LookupOptions`](crate::LookupOptions) and
  [`FrameLookupOptions`](crate::FrameLookupOptions) switch off what you do not
  need. Asking for names only skips source lines, inline frames, and call
  sites.

```rust
use gsym::{
    AddressRange, FrameLookupOptions, Function, Gsym, GsymBuilder, LookupScratch,
};

let mut builder = GsymBuilder::new();
builder.add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"hot"))?;
let bytes = builder.to_bytes()?;
let gsym = Gsym::parse(&bytes)?;

// One scratch buffer, reused across every address on this thread.
let mut scratch = LookupScratch::with_capacity(16);
let options = FrameLookupOptions {
    line_information: false,
    inline_frames: false,
};

let mut resolved = 0_usize;
for address in [0x1000, 0x1004, 0x100c] {
    gsym.for_each_frame(address, options, &mut scratch, |_frame| resolved += 1)?;
}
assert_eq!(resolved, 3);
# Ok::<(), gsym::Error>(())
```

Lookup cost grows with the metadata of the matched function, not with the size
of the file.

`Gsym<D>` is `Send` and `Sync` whenever its storage is. Lookups take `&self` and
hold no interior mutable state, and there is no global cache and no lock. Share
one reader across threads behind an `Arc`, or build one once into a `static`, and
give each thread its own `LookupScratch`.

## Untrusted input

A malformed file produces an [`Error`](crate::Error) rather than a panic.
Parsing checks the file's structure, and a bad function record is reported by
the lookup that reads it.

To find that out up front instead, [`Gsym::verify`](crate::Gsym::verify) checks
every function and everything it references, and returns what it counted.

```rust
use gsym::{AddressRange, Function, Gsym, GsymBuilder};

let mut builder = GsymBuilder::new();
builder.add_function(Function::new(AddressRange::new(0x1000, 0x1010), b"checked"))?;
let bytes = builder.to_bytes()?;

let gsym = Gsym::parse(&bytes)?;
let report = gsym.verify()?;
assert_eq!(report.functions, 1);
# Ok::<(), gsym::Error>(())
```

Verification cost is proportional to the file, so it belongs at load time rather
than in front of each lookup.

## Further reading

Elsewhere in these docs:

- [Format](crate::docs::format) for what a GSYM file contains
- [Cookbook](crate::docs::cookbook) for worked examples of every API used above

For the runtime half of the problem, which this crate does not solve:

- [`dl_iterate_phdr(3)`](https://man7.org/linux/man-pages/man3/dl_iterate_phdr.3.html)
  enumerates loaded modules and reports the load bias (`dlpi_addr`) and program
  headers needed to unslide an address
- [`proc(5)`](https://man7.org/linux/man-pages/man5/proc.5.html) documents
  `/proc/self/maps`, the coarser way to find which module owns an address
- [GDB: separate debug files](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Separate-Debug-Files.html)
  describes how build IDs identify a binary and its debug information

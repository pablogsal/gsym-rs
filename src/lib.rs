//! Reader, writer, and Linux ELF/DWARF converter for [LLVM GSYM], in safe Rust.
//!
//! GSYM maps an instruction address to a function name, a source file and line,
//! and the chain of inlined calls at that address. It stores nothing else, so a
//! GSYM file is far smaller than the DWARF it was built from, and its sorted
//! address index can be memory-mapped and queried without parsing the rest of
//! the file.
//!
//! # Quick start
//!
//! Build a file in memory and resolve an address:
//!
//! ```
//! use gsym::{AddressRange, FileEntry, Function, Gsym, GsymBuilder, LineEntry};
//!
//! let mut builder = GsymBuilder::new().base_address(0x4000);
//! let source = builder.add_file(FileEntry::new(b"src", b"main.rs"))?;
//! builder.add_function(Function {
//!     lines: vec![LineEntry::new(0x4010, source, 12)],
//!     ..Function::new(AddressRange::new(0x4010, 0x4020), b"example")
//! })?;
//! let bytes = builder.to_bytes()?;
//!
//! let gsym = Gsym::parse(&bytes)?;
//! let hit = gsym.lookup(0x4014)?.expect("address is covered");
//! assert_eq!(hit.frames()[0].name, b"example");
//! assert_eq!(hit.frames()[0].line, 12);
//! assert_eq!(hit.frames()[0].offset, 4);
//! # Ok::<(), gsym::Error>(())
//! ```
//!
//! Read a file written by this crate or by `llvm-gsymutil`:
//!
//! ```no_run
//! use gsym::Gsym;
//!
//! let gsym = Gsym::open("app.gsym")?;
//! if let Some(hit) = gsym.lookup(0x401120)? {
//!     for frame in hit.frames() {
//!         println!(
//!             "{}{} at {}:{}",
//!             String::from_utf8_lossy(frame.name),
//!             if frame.inlined { " (inlined)" } else { "" },
//!             String::from_utf8_lossy(frame.basename),
//!             frame.line,
//!         );
//!     }
//! }
//! # Ok::<(), gsym::Error>(())
//! ```
//!
//! Build one from an executable's own DWARF (`convert` feature, on by default):
//!
//! ```no_run
//! # #[cfg(feature = "convert")]
//! # fn run() -> gsym::Result<()> {
//! use gsym::convert::ElfConverter;
//!
//! let report = ElfConverter::default().convert_path("./app")?;
//! std::fs::write("./app.gsym", report.builder.to_bytes()?)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Choosing an entry point
//!
//! | To … | Use | Notes |
//! | --- | --- | --- |
//! | query a file on disk | [`Gsym::open`] | safe; reads one owned snapshot |
//! | query bytes you already hold | [`Gsym::parse`] | any `AsRef<[u8]>`, borrowed or owned, no copy |
//! | query a large file without reading it all | `MappedGsym::map` | `mmap` feature; `unsafe` |
//! | create a file | [`GsymBuilder`] | deterministic output from semantic records |
//! | import ELF and DWARF | `convert::ElfConverter` | `convert` feature |
//! | change version or byte order | [`Gsym::transcode`] | or [`DecodedGsym`] to edit in between |
//! | split into shards | [`DecodedGsym::segments`] | size-bounded, independently readable |
//! | check an untrusted file | [`Gsym::verify`] | checks the whole file up front |
//!
//! All readers are the same type, [`Gsym<D>`](Gsym) over different byte
//! storage, so the query API does not change with the choice.
//!
//! # Usage notes
//!
//! * Addresses are unslid. A GSYM file records the virtual addresses of the ELF
//!   image it was built from, so an address captured from a running PIE
//!   executable or shared object must have its load bias subtracted first.
//!   Skipping that step is the usual cause of an empty or wrong result; see
//!   [`docs::symbolication`].
//!
//! * Names and paths are bytes. The format stores raw bytes and this crate
//!   preserves them, so results are `&[u8]`. Use `String::from_utf8_lossy` to
//!   display them, or `str::from_utf8` when invalid data should be reported.
//!
//! * Version 1 is the default and is what current tooling reads. Version 2
//!   raises v1's 4 GiB offset limits and its 20-byte build-ID limit but needs
//!   LLVM 23 or newer, so it must be requested with [`GsymVersion::V2`].
//!
//! * Lookups take `&self` and keep no interior state. There is no global cache
//!   and no lock, so one reader can be shared across threads. The one reusable
//!   buffer, [`LookupScratch`], belongs to the caller.
//!
//! # Feature flags
//!
//! | Feature | Default | Adds |
//! | --- | --- | --- |
//! | `mmap` | yes | `MappedGsym`, a reader backed by a read-only memory map |
//! | `convert` | yes | the `convert` module: Linux ELF and DWARF import |
//! | `debuginfod` | no | debuginfod network lookup during conversion |
//!
//! The codec needs neither default feature:
//!
//! ```toml
//! [dependencies]
//! gsym-rs = { version = "0.1", default-features = false }
//! ```
//!
//! # Errors
//!
//! Fallible entry points return [`Result<T>`](Result). Its error type is
//! [`Error`], which covers malformed input, model violations, exceeded limits,
//! I/O, and, with `convert`, ELF and DWARF diagnostics. It is
//! `#[non_exhaustive]`, so a match on it needs a fallback arm.
//!
//! Lookup validates only the records it reads, so an untrusted file is worth
//! one [`Gsym::verify`] at load time.
//!
//! # Guides
//!
//! - [`docs::symbolication`]: unslid addresses, reading frames, storage choices,
//!   performance, threading.
//! - [`docs::cookbook`]: worked examples for every part of the API.
#![cfg_attr(
    feature = "convert",
    doc = " - [`docs::conversion`]: building GSYM from Linux ELF files and their DWARF."
)]
//! - [`docs::format`]: the on-disk format, version 1 and version 2.
//!
//! [LLVM GSYM]: https://llvm.org/doxygen/namespacellvm_1_1gsym.html

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod docs;

mod builder;
mod endian;
mod error;
mod format;
mod model;
mod normalize;
mod reader;
mod transform;
mod validation;
mod version;
mod writer;

#[cfg(feature = "convert")]
#[cfg_attr(docsrs, doc(cfg(feature = "convert")))]
/// Linux ELF and DWARF conversion support.
pub mod convert;
#[cfg(feature = "mmap")]
#[expect(
    unsafe_code,
    reason = "memory mapping a file is inherently unsafe and is confined to this module"
)]
mod mapped;

pub use builder::{BuilderOptions, FunctionSetPolicy, GsymBuilder};
pub use endian::Endian;
#[cfg(feature = "convert")]
#[cfg_attr(docsrs, doc(cfg(feature = "convert")))]
pub use error::{CompanionMismatch, ElfInputKind, ParserError};
pub use error::{Error, Result};
pub use model::{
    AddressRange, CallSite, CallSiteFlags, FileEntry, FileIndex, Function, InlineNode, LineEntry,
    Lookup, LookupFrame,
};
pub use reader::{
    FrameLookupOptions, FunctionRef, Functions, Gsym, Header, LookupOptions, LookupScratch,
    VerifyReport,
};
pub use transform::{DecodedGsym, GsymSegment, TranscodeOptions};
pub use version::GsymVersion;
pub use writer::WriterOptions;

#[cfg(feature = "mmap")]
#[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
pub use mapped::{MappedBytes, MappedGsym};

/// Compiles the README's examples as doctests without rendering it twice.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

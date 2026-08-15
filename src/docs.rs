//! Long-form guides.
//!
//! - [`symbolication`]: which address to ask about, how to read the frames that
//!   come back, and what a lookup costs.
//! - [`cookbook`]: worked examples for each part of the public API.
#![cfg_attr(
    feature = "convert",
    doc = " - [`conversion`]: building GSYM from Linux ELF files and their DWARF."
)]
//! - [`mod@format`]: the on-disk format, for debugging files and comparing
//!   against `llvm-gsymutil`.

#[doc = include_str!("../docs/symbolication.md")]
pub mod symbolication {}

#[doc = include_str!("../docs/cookbook.md")]
pub mod cookbook {}

#[cfg(feature = "convert")]
#[cfg_attr(docsrs, doc(cfg(feature = "convert")))]
#[doc = include_str!("../docs/cookbook-convert.md")]
pub mod conversion {}

#[doc = include_str!("../docs/format.md")]
pub mod format {}

//!
//! [`ElfConverter`](crate::convert::ElfConverter) reads an ELF image, imports
//! its `STT_FUNC` symbols and DWARF subprogram information, and returns a
//! populated [`GsymBuilder`](crate::GsymBuilder) rather than finished bytes, so
//! the model can be inspected or edited before an encoding is chosen.
//!
//! ```no_run
//! use gsym::convert::ElfConverter;
//!
//! let report = ElfConverter::default().convert_path("./app")?;
//! std::fs::write("./app.gsym", report.builder.to_bytes()?)?;
//! # Ok::<(), gsym::Error>(())
//! ```
//!
//! [`convert_path`](crate::convert::ElfConverter::convert_path) also searches
//! for separate debug files: `.gnu_debuglink` targets validated by CRC,
//! build-ID trees, debuginfod when it is configured, embedded
//! `.gnu_debugdata`, supplementary `.gnu_debugaltlink` objects, and split
//! DWARF. [`convert`](crate::convert::ElfConverter::convert) takes
//! caller-supplied [`ElfInputs`](crate::convert::ElfInputs) instead and
//! searches for nothing.
//!
//! Conversion is lenient. Anything unusable is skipped, counted in
//! [`ConversionStats`](crate::convert::ConversionStats), and explained in a
//! [`ConversionWarning`](crate::convert::ConversionWarning), so one bad
//! compilation unit does not fail the whole file. Errors are reserved for input
//! that cannot be used at all, such as a malformed image or a companion file
//! belonging to a different build.
//!
//! See [`docs::conversion`](crate::docs::conversion) for the full guide.

mod diagnostic;
mod dwarf;
mod elf;

use object::{ObjectSection, SectionKind};

pub use diagnostic::ConversionWarning;
pub use elf::{
    ConversionOptions, ConversionReport, ConversionStats, DiscoveryPolicy, DwarfImportOptions,
    ElfConverter, ElfInputs,
};

fn is_debug_section(section: &object::Section<'_, '_>) -> bool {
    section.kind() == SectionKind::Debug
        || section
            .name_bytes()
            .is_ok_and(|name| name.starts_with(b".debug_") || name.starts_with(b".zdebug_"))
}

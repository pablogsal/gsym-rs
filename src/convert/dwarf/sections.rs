use std::borrow::Cow;
use std::collections::HashMap;

use gimli::{ReaderOffset, Relocate, SectionId};
use object::{
    Object, ObjectSection, ObjectSymbol, RelocationEncoding, RelocationKind, RelocationTarget,
};

use super::super::elf::AddressLayout;
use super::super::is_debug_section;
use crate::{Error, Result};

#[derive(Debug)]
pub(super) struct SectionData<'data> {
    pub(super) data: Cow<'data, [u8]>,
    pub(super) relocations: DwarfRelocations,
}

impl Default for SectionData<'_> {
    fn default() -> Self {
        Self {
            data: Cow::Borrowed(&[]),
            relocations: DwarfRelocations::default(),
        }
    }
}

impl SectionData<'_> {
    fn into_owned(self) -> SectionData<'static> {
        SectionData {
            data: Cow::Owned(self.data.into_owned()),
            relocations: self.relocations,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct DwarfRelocations(pub(super) HashMap<u64, RelocationEntry>);

#[derive(Clone, Copy, Debug)]
pub(super) struct RelocationEntry {
    pub(super) implicit_addend: bool,
    pub(super) value: u64,
}

impl Relocate for &DwarfRelocations {
    fn relocate_address(&self, offset: usize, value: u64) -> gimli::Result<u64> {
        Ok(self.apply(offset, value))
    }

    fn relocate_offset(&self, offset: usize, value: usize) -> gimli::Result<usize> {
        usize::from_u64(self.apply(offset, value as u64))
    }
}

impl DwarfRelocations {
    pub(super) fn apply(&self, offset: usize, value: u64) -> u64 {
        self.0.get(&(offset as u64)).map_or(value, |relocation| {
            if relocation.implicit_addend {
                value.wrapping_add(relocation.value)
            } else {
                relocation.value
            }
        })
    }

    fn load(
        file: &object::File<'_>,
        section: &object::Section<'_, '_>,
        layout: &AddressLayout,
    ) -> Result<Self> {
        let mut output = HashMap::new();
        for (offset, relocation) in section.relocations() {
            if relocation.kind() == RelocationKind::None {
                continue;
            }
            let kind = relocation.kind();
            if !matches!(
                kind,
                RelocationKind::Absolute | RelocationKind::Relative | RelocationKind::SectionOffset
            ) || !debug_relocation_encoding_supported(kind, relocation.encoding())
            {
                return Err(Error::malformed(
                    "DWARF relocation",
                    format!(
                        "unsupported kind/encoding {:?}/{:?} at {offset:#x}",
                        relocation.kind(),
                        relocation.encoding()
                    ),
                ));
            }
            let (target, target_section_base) = match relocation.target() {
                RelocationTarget::Symbol(index) => {
                    let symbol = file.symbol_by_index(index).map_err(|error| {
                        Error::malformed("DWARF relocation symbol", error.to_string())
                    })?;
                    let mut value = symbol.address();
                    let mut section_base = 0;
                    if let Some(index) = symbol.section_index() {
                        let target_section = file.section_by_index(index).map_err(|error| {
                            Error::malformed("DWARF relocation section", error.to_string())
                        })?;
                        if !is_debug_section(&target_section) {
                            section_base = layout.section_base(&target_section)?;
                            value = section_base
                                .checked_add(value)
                                .ok_or(Error::Overflow("debug relocation target"))?;
                        }
                    }
                    (value, section_base)
                }
                RelocationTarget::Section(index) => {
                    let target_section = file.section_by_index(index).map_err(|error| {
                        Error::malformed("DWARF relocation section", error.to_string())
                    })?;
                    if is_debug_section(&target_section) {
                        (0, 0)
                    } else {
                        let base = layout.section_base(&target_section)?;
                        (base, base)
                    }
                }
                RelocationTarget::Absolute => {
                    return Err(Error::malformed(
                        "DWARF relocation",
                        format!("unsupported target at {offset:#x}"),
                    ));
                }
                _ => {
                    return Err(Error::malformed(
                        "DWARF relocation",
                        format!("unrecognised target at {offset:#x}"),
                    ));
                }
            };
            let place = section.address().wrapping_add(offset);
            let value = calculate_relocation_value(
                kind,
                target,
                target_section_base,
                place,
                relocation.addend(),
            )?;
            if output
                .insert(
                    offset,
                    RelocationEntry {
                        implicit_addend: relocation.has_implicit_addend(),
                        value,
                    },
                )
                .is_some()
            {
                return Err(Error::malformed(
                    "DWARF relocation",
                    format!("multiple entries at {offset:#x}"),
                ));
            }
        }
        Ok(Self(output))
    }
}

pub(super) fn debug_relocation_encoding_supported(
    kind: RelocationKind,
    encoding: RelocationEncoding,
) -> bool {
    encoding == RelocationEncoding::Generic
        || (kind == RelocationKind::Absolute && encoding == RelocationEncoding::X86Signed)
}

pub(super) fn calculate_relocation_value(
    kind: RelocationKind,
    target: u64,
    target_section_base: u64,
    place: u64,
    addend: i64,
) -> Result<u64> {
    let value = match kind {
        RelocationKind::Absolute => target,
        RelocationKind::Relative => target.wrapping_sub(place),
        RelocationKind::SectionOffset => target.wrapping_sub(target_section_base),
        RelocationKind::Unknown
        | RelocationKind::None
        | RelocationKind::Got
        | RelocationKind::GotRelative
        | RelocationKind::GotBaseRelative
        | RelocationKind::GotBaseOffset
        | RelocationKind::PltRelative
        | RelocationKind::ImageOffset
        | RelocationKind::SectionIndex => {
            return Err(Error::malformed(
                "DWARF relocation",
                format!("unsupported kind {kind:?}"),
            ));
        }
        _ => {
            return Err(Error::malformed(
                "DWARF relocation",
                format!("unrecognised kind {kind:?}"),
            ));
        }
    };
    Ok(value.wrapping_add(addend.cast_unsigned()))
}

pub(super) fn load_section<'data>(
    file: &object::File<'data>,
    id: SectionId,
    layout: &AddressLayout,
) -> Result<SectionData<'data>> {
    load_named_section(file, id.name(), layout)
}

pub(super) fn load_dwo_section<'data>(
    file: &object::File<'data>,
    id: SectionId,
    layout: &AddressLayout,
) -> Result<SectionData<'data>> {
    let Some(name) = id.dwo_name() else {
        return Ok(SectionData::default());
    };
    load_named_section(file, name, layout)
}

pub(super) fn load_dwo_section_owned(
    file: &object::File<'_>,
    id: SectionId,
    layout: &AddressLayout,
) -> Result<SectionData<'static>> {
    load_dwo_section(file, id, layout).map(SectionData::into_owned)
}

fn load_named_section<'data>(
    file: &object::File<'data>,
    name: &str,
    layout: &AddressLayout,
) -> Result<SectionData<'data>> {
    let Some(section) = file.section_by_name(name) else {
        return Ok(SectionData::default());
    };
    let data = section.uncompressed_data().map_err(|error| {
        Error::malformed("DWARF section", format!("failed to read {name}: {error}"))
    })?;
    let relocations = DwarfRelocations::load(file, &section, layout)?;
    Ok(SectionData { data, relocations })
}
